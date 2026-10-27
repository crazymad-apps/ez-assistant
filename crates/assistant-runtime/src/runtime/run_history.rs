//! 文件中的本会话真实 Run 直接投影产品值；继承记录只在 Conversation 展示。

use std::collections::{HashMap, HashSet};

use agent_types::{AssistantMessage, AssistantPart, ConversationMessage, ConversationSnapshot};
use assistant_protocol::{ConversationOwner, McpToolIdentity, RunSnapshot, RunStatus, SessionId};

use super::AssistantRuntime;
use crate::{
    RuntimeError, RuntimeResult, StoredHistoryFacts, StoredRunHistory, session::SessionController,
};

impl AssistantRuntime {
    /// 幂等准入仍由已接受 Input 证明；终态返回值直接读取文件，不恢复执行句柄。
    pub(super) async fn find_idempotent_run(
        &self,
        session: &SessionController,
        key: &assistant_protocol::IdempotencyKey,
    ) -> RuntimeResult<Option<(assistant_protocol::InputId, Option<RunSnapshot>)>> {
        let Some((input_id, run_id)) = session.find_idempotent(key)? else {
            return Ok(None);
        };
        let Some(run_id) = run_id else {
            return Ok(Some((input_id, None)));
        };
        let run = self
            .product_run_snapshots(session)
            .await?
            .into_iter()
            .find(|run| run.run_id == run_id && run.input_id == input_id)
            .ok_or(RuntimeError::InternalStateUnavailable {
                component: "idempotent Run history",
            })?;
        Ok(Some((input_id, Some(run))))
    }
    /// 读取当前 owner/generation 的可靠历史，活动执行只补充实时观察。
    ///
    /// 不安装历史执行对象、不写 Store。调用方可持 Session mutation；本函数只短暂读取
    /// 状态锁，不重入 mutation，Store 与 MCP 查询均在状态锁外完成。文件终态一旦可读，
    /// 旧执行记录只能补充同一工具的易失输出，不能覆盖可靠字段；读取失败原样返回。
    pub(super) async fn product_run_snapshots(
        &self,
        session: &SessionController,
    ) -> RuntimeResult<Vec<RunSnapshot>> {
        // 原有不可读正文仍允许展示已装配的执行诊断，不把文件读取失败当成空历史。
        if !session.lock_state()?.is_conversation_available {
            return session.run_snapshots();
        }
        let history = self
            .store
            .load_conversation_history(ConversationOwner::MainSession {
                session_id: session.id().clone(),
            })
            .await
            .map_err(|source| RuntimeError::from_store("load Run history", source))?;
        // Journal 仅保留当前模型上下文，不能用于完整历史；也不能在持 mutation 的幂等/
        // 取消路径中借懒加载重入该 gate。直接读原件用于投影，不安装或恢复执行对象。
        let conversation = self
            .store
            .load_conversation(session.id())
            .await
            .map_err(|source| RuntimeError::from_store("load Run conversation", source))?;
        self.project_product_run_snapshots(session, &history, &conversation)
    }

    /// 同一次可靠读取可供 Run、资源引用与上下文计量共用；不再次加载完整正文。
    ///
    /// # Errors
    /// 归属／代次不一致或投影失败时返回错误。
    pub(super) fn project_product_run_snapshots(
        &self,
        session: &SessionController,
        history: &crate::StoredConversationHistory,
        conversation: &ConversationSnapshot,
    ) -> RuntimeResult<Vec<RunSnapshot>> {
        let mcp_identities = self.current_mcp_tool_identities()?;
        let state = session.lock_state()?;
        if history.generation != state.body_generation {
            return Err(RuntimeError::SnapshotStale);
        }
        let inherited: HashSet<_> = history
            .facts
            .runs
            .iter()
            .filter(|run| run.inherited || run.partial)
            .map(|run| &run.run_id)
            .collect();
        let mut runs: HashMap<_, _> =
            project_history_runs(session.id(), &history.facts, conversation, &mcp_identities)?
                .into_iter()
                .map(|run| (run.run_id.clone(), run))
                .collect();
        for (id, run) in &state.runs {
            if !inherited.contains(id) {
                let observation = run.snapshot();
                if let Some(reliable) = runs.get_mut(id)
                    && reliable.status.is_terminal()
                {
                    // stdout/stderr 没有可靠历史原件；只补已证明是同一次工具调用的观察。
                    for tool in &mut reliable.tools {
                        if tool.step.is_some()
                            && let Some(observed) = observation.tools.iter().find(|observed| {
                                observed.step == tool.step
                                    && observed.call_id == tool.call_id
                                    && observed.tool_name == tool.tool_name
                            })
                        {
                            tool.stdout.clone_from(&observed.stdout);
                            tool.stderr.clone_from(&observed.stderr);
                        }
                    }
                } else if !observation.status.is_terminal()
                    || !state.execution_prepared && observation.status == RunStatus::Interrupted
                {
                    // 尚未 prepare 的旧进程活动控制保留既有 interrupted 诊断；首次执行
                    // prepare 才可靠结算。该观察不得覆盖已经提交的文件终态。
                    runs.insert(id.clone(), observation);
                }
            }
        }
        let mut order = HashMap::new();
        for input in history.facts.inputs.iter().filter(|input| !input.inherited) {
            order.insert(input.input_id.clone(), input.queue_order);
        }
        for input in state.inputs.values() {
            order
                .entry(input.stored.input_id.clone())
                .or_insert(input.stored.queue_order);
        }
        let mut fallback_created = HashMap::new();
        for run in runs.values() {
            let created = run.created_at_ms.unwrap_or(i64::MAX);
            fallback_created
                .entry(run.input_id.clone())
                .and_modify(|value: &mut i64| *value = (*value).min(created))
                .or_insert(created);
        }
        let mut runs: Vec<_> = runs.into_values().collect();
        runs.sort_by(|left, right| {
            (
                order.get(&left.input_id).copied().unwrap_or(u64::MAX),
                fallback_created[&left.input_id],
                left.attempt,
                left.run_id.as_str(),
            )
                .cmp(&(
                    order.get(&right.input_id).copied().unwrap_or(u64::MAX),
                    fallback_created[&right.input_id],
                    right.attempt,
                    right.run_id.as_str(),
                ))
        });
        Ok(runs)
    }
}

fn project_history_runs(
    session_id: &SessionId,
    facts: &StoredHistoryFacts,
    conversation: &ConversationSnapshot,
    mcp_identities: &HashMap<(String, String), McpToolIdentity>,
) -> RuntimeResult<Vec<RunSnapshot>> {
    let owners: HashMap<_, _> = facts
        .message_origins
        .iter()
        .filter_map(|origin| origin.run_id.as_ref().map(|run| (&origin.message_id, run)))
        .collect();
    let mut finals = HashMap::new();
    for message in &conversation.messages {
        if let ConversationMessage::Assistant(message) = message
            && message.purpose.is_model_response()
            && let Some(run) = owners.get(&message.id)
        {
            finals.insert(*run, message);
        }
    }
    let mut tools = super::product::project_run_tool_activities(
        conversation,
        &facts.message_origins,
        mcp_identities,
    )?;
    Ok(facts
        .runs
        .iter()
        .filter_map(|run| {
            let mut snapshot = project_run(session_id, run, finals.get(&run.run_id).copied())?;
            snapshot.tools = tools.remove(&run.run_id).unwrap_or_default();
            Some(snapshot)
        })
        .collect())
}

fn project_run(
    session_id: &SessionId,
    run: &StoredRunHistory,
    final_assistant: Option<&AssistantMessage>,
) -> Option<RunSnapshot> {
    if run.inherited || run.partial {
        return None;
    }
    let mut reasoning_parts = Vec::new();
    let mut text = String::new();
    if let Some(message) = final_assistant {
        for part in &message.parts {
            match part {
                AssistantPart::Reasoning(part) => {
                    reasoning_parts.push(crate::run::project_reasoning_part(part))
                }
                AssistantPart::Text(part) => text.push_str(&part.text),
                AssistantPart::ToolCall(_) | AssistantPart::ProviderState(_) => {}
            }
        }
    }
    Some(RunSnapshot {
        run_id: run.run_id.clone(),
        session_id: session_id.clone(),
        input_id: run.input_id.clone()?,
        attempt: run.attempt,
        created_at_ms: Some(run.created_at_ms),
        started_at_ms: run.started_at_ms,
        finished_at_ms: run.finished_at_ms,
        status: run.status?,
        variant: run.agent_variant,
        approval_mode: run.approval_mode,
        reasoning_effort: run.reasoning_effort,
        cancel_requested: run.cancel_requested,
        active_step: None,
        model_request: None,
        reasoning_parts,
        text,
        tools: Vec::new(),
        error: run.error.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_types::{
        AssistantMessagePurpose, FinishReason, MessageId, ModelIdentity, PartId, ProviderId,
        TextPart,
    };
    use assistant_protocol::{AgentVariant, ApprovalMode, InputId, RunId, RunStatus};

    fn fact() -> StoredRunHistory {
        StoredRunHistory {
            finished_after_message_id: None,
            run_id: RunId::new("local-run").unwrap(),
            input_id: Some(InputId::new("local-input").unwrap()),
            user_message_id: None,
            attempt: 2,
            agent_variant: AgentVariant::Build,
            approval_mode: ApprovalMode::Ask,
            reasoning_effort: None,
            agent_shell_kind: None,
            status: Some(RunStatus::Failed),
            error: None,
            cancel_requested: false,
            created_at_ms: 10,
            started_at_ms: Some(20),
            finished_at_ms: Some(30),
            inherited: false,
            partial: false,
            last_message_id: None,
        }
    }

    fn assistant(id: &str, text: &str) -> ConversationMessage {
        ConversationMessage::Assistant(AssistantMessage {
            id: MessageId::new(id).unwrap(),
            purpose: AssistantMessagePurpose::ModelResponse,
            model: ModelIdentity::new(ProviderId::new("fixture").unwrap(), "fixture"),
            parts: vec![AssistantPart::Text(TextPart {
                id: PartId::new(format!("part-{id}")).unwrap(),
                text: text.into(),
            })],
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }

    #[test]
    fn read_only_facts_project_last_owned_message_without_an_executable_record() {
        let run = fact();
        let facts = StoredHistoryFacts {
            runs: vec![run.clone()],
            message_origins: ["first", "last"]
                .into_iter()
                .map(|id| crate::StoredMessageOrigin {
                    message_id: MessageId::new(id).unwrap(),
                    run_id: Some(run.run_id.clone()),
                    input_id: run.input_id.clone(),
                    attempt: Some(2),
                    step: Some(1),
                    inherited: false,
                })
                .collect(),
            ..Default::default()
        };
        let result = project_history_runs(
            &SessionId::new("owner").unwrap(),
            &facts,
            &ConversationSnapshot::new(vec![
                assistant("first", "old"),
                assistant("last", "reliable"),
                assistant("other-run", "must not leak"),
            ]),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "reliable");
        assert_eq!(result[0].status, RunStatus::Failed);
        assert_eq!(result[0].session_id.as_str(), "owner");
        assert_eq!(result[0].finished_at_ms, Some(30));
        assert!(result[0].model_request.is_none());
    }

    #[test]
    fn inherited_partial_or_unknown_input_never_becomes_a_controllable_run_snapshot() {
        let session = SessionId::new("owner").unwrap();
        let mut run = fact();
        run.inherited = true;
        assert!(project_run(&session, &run, None).is_none());
        run.inherited = false;
        run.partial = true;
        assert!(project_run(&session, &run, None).is_none());
        run.partial = false;
        run.input_id = None;
        assert!(project_run(&session, &run, None).is_none());
        run = fact();
        let zero_message = project_run(&session, &run, None).unwrap();
        assert!(zero_message.text.is_empty());
        assert_eq!(zero_message.attempt, 2);
        assert_eq!(zero_message.status, RunStatus::Failed);
    }

    #[test]
    fn historical_tools_pair_by_assistant_and_keep_input_sanitization() {
        let run = fact();
        let mut messages = Vec::new();
        let mut origins = Vec::new();
        for (step, id, status) in [
            (1, "first", agent_types::ToolResultStatus::Error),
            (2, "second", agent_types::ToolResultStatus::Success),
        ] {
            let ConversationMessage::Assistant(mut assistant) = assistant(id, "") else {
                unreachable!()
            };
            let call_id = agent_types::ToolCallId::new("same-call").unwrap();
            assistant.parts = vec![AssistantPart::ToolCall(agent_types::ToolCall {
                id: call_id.clone(),
                name: agent_types::ToolName::new("echo_tool").unwrap(),
                arguments: serde_json::json!({"api_key": "do-not-expose", "step": step}),
            })];
            origins.push(crate::StoredMessageOrigin {
                message_id: assistant.id.clone(),
                run_id: Some(run.run_id.clone()),
                input_id: run.input_id.clone(),
                attempt: Some(run.attempt),
                step: Some(step),
                inherited: false,
            });
            messages.push(ConversationMessage::Assistant(assistant));
            messages.push(ConversationMessage::Tool(agent_types::ToolMessage {
                id: MessageId::new(format!("result-{id}")).unwrap(),
                result: agent_types::ToolResult {
                    call_id,
                    status,
                    content: agent_types::ToolResultContent::text("not a stdout stream"),
                    metadata: None,
                },
            }));
        }
        let snapshots = project_history_runs(
            &SessionId::new("owner").unwrap(),
            &StoredHistoryFacts {
                runs: vec![run],
                message_origins: origins,
                ..Default::default()
            },
            &ConversationSnapshot::new(messages),
            &HashMap::new(),
        )
        .unwrap();
        let tools = &snapshots[0].tools;
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools[0].status,
            assistant_protocol::ToolActivityStatus::Failed
        );
        assert_eq!(
            tools[1].status,
            assistant_protocol::ToolActivityStatus::Completed
        );
        assert_eq!(tools[0].step, Some(1));
        assert_eq!(tools[1].step, Some(2));
        for tool in tools {
            assert!(tool.input.redacted);
            assert!(!format!("{:?}", tool.input).contains("do-not-expose"));
            assert!(tool.stdout.is_empty());
            assert!(tool.stderr.is_empty());
        }
    }
}
