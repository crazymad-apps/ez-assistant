//! 文件历史与旧 SQL 来源之间的迁出边界；正常历史不回写为 SQL 关联。

use std::collections::{HashMap, HashSet};

use agent_types::{ConversationMessage, ConversationSnapshot, UserPart};
use assistant_protocol::{ConversationOwner, RunStatus};
use assistant_runtime::{
    StoredChildTaskHistory, StoredHistoryAttachment, StoredHistoryFacts, StoredHistoryFeedback,
    StoredMessageOrigin, StoredRunHistory, StoredSessionCommandState,
};

use super::{
    StorageEngine, StorageResult,
    append_effect::{AppendPurpose, ConversationStorageTarget},
    body_path, conflict, conversation, internal_error, invalid_data,
};

/// 迁出／裁尾只保留消息、调用及受管资源的定位键，不保留正文或工具输出。
#[derive(Default)]
pub(super) struct HistoryMessageKeys {
    pub(super) messages: HashMap<agent_types::MessageId, usize>,
    calls: HashSet<String>,
    attachment_paths: HashSet<String>,
}

impl HistoryMessageKeys {
    pub(super) fn record(&mut self, message: &ConversationMessage) -> StorageResult<()> {
        let ordinal = self.messages.len();
        if self
            .messages
            .insert(conversation::message_id(message).clone(), ordinal)
            .is_some()
        {
            return Err(invalid_data("conversation contains a duplicate message id"));
        }
        match message {
            ConversationMessage::Assistant(assistant) => {
                self.calls
                    .extend(assistant.parts.iter().filter_map(|part| match part {
                        agent_types::AssistantPart::ToolCall(call) => {
                            Some(call.id.as_str().to_owned())
                        }
                        _ => None,
                    }));
            }
            ConversationMessage::User(user) => {
                self.attachment_paths.extend(
                    user.parts
                        .iter()
                        .filter_map(|part| match part {
                            UserPart::FileReferences(files) => Some(&files.files),
                            _ => None,
                        })
                        .flatten()
                        .map(|file| file.readable_path.clone()),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn from_conversation(conversation: &ConversationSnapshot) -> StorageResult<Self> {
        let mut keys = Self::default();
        for message in &conversation.messages {
            keys.record(message)?;
        }
        Ok(keys)
    }
}

impl StorageEngine {
    /// 只读取来源及顺序，不解码 queued_message_json，也不创建可执行 StoredInput。
    fn read_input_history(
        &self,
        session_id: &assistant_protocol::SessionId,
        input_id: Option<&assistant_protocol::InputId>,
    ) -> StorageResult<Vec<assistant_runtime::StoredInputHistory>> {
        let mut statement = self.connection.prepare(
            "SELECT json_object(
                'input_id',input_id,'user_message_id',user_message_id,'queue_order',queue_order,
                'agent_variant',agent_variant,'origin',origin,'accepted_at_ms',accepted_at_ms,
                'agent_shell_target',agent_shell_target,'inherited',json('false'),
                'goal_binding',CASE WHEN goal_id IS NULL AND goal_generation IS NULL AND goal_turn IS NULL AND goal_reply_route_json IS NULL
                    THEN NULL ELSE json_object('goal_id',goal_id,'generation',goal_generation,'turn',goal_turn,'reply_route',json(goal_reply_route_json)) END,
                'cross_session',json(cross_session_json),'channel_source',json(channel_source_json))
             FROM inputs WHERE session_id=?1 AND input_kind='message' AND (?2 IS NULL OR input_id=?2)
               AND (?2 IS NOT NULL OR state='committed')
             ORDER BY queue_order,input_id"
        ).map_err(|e| internal_error("history Input metadata could not be queried", e))?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    session_id.as_str(),
                    input_id.map(assistant_protocol::InputId::as_str)
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| internal_error("history Input metadata could not be read", e))?;
        rows.map(|row| {
            let json =
                row.map_err(|e| internal_error("history Input metadata row is invalid", e))?;
            let mut input: assistant_runtime::StoredInputHistory = serde_json::from_str(&json)
                .map_err(|e| {
                    super::invalid_data_with_source("history Input metadata is invalid", e)
                })?;
            if input.origin == assistant_runtime::InputOrigin::User
                && input.channel_source.is_none()
            {
                // 与已发布 Input loader 相同的旧 Desktop 文本语义，不修改排队原件。
                input.channel_source = Some(assistant_runtime::InputChannelSource::desktop_text());
            }
            Ok(input)
        })
        .collect()
    }
    /// 当前 owner 的事实随正文换代；旧无头来源先迁出关联，已有文件事实不另建数据库副本。
    /// 调用者仍持 owner gate，尚未发布新 generation；失败不改变旧文件和当前指针。
    pub(super) fn history_for_replacement(
        &mut self,
        owner: &ConversationOwner,
        replacement: &ConversationSnapshot,
    ) -> StorageResult<StoredHistoryFacts> {
        self.ensure_no_pending_history(owner)?;
        let (generation, path) = match owner {
            ConversationOwner::MainSession { session_id } => {
                let generation = self.session_generation(session_id)?;
                (
                    generation,
                    body_path(&self.session_directory(session_id)?, generation),
                )
            }
            ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } => {
                let generation = self.child_generation(session_id, child_task_id)?;
                (
                    generation,
                    self.child_body(session_id, child_task_id, generation)?,
                )
            }
        };
        let document = self.read_owner_document(&path, owner, generation)?;
        let previous: HashSet<_> = document
            .conversation
            .messages
            .iter()
            .map(conversation::message_id)
            .collect();
        let mut history = match document.history {
            Some(history) => history,
            None => self.legacy_history_facts(owner, &document.conversation)?,
        };
        retain_replacement_facts(&mut history, owner, replacement)?;
        // 真正新增的上下文消息也是本 owner 的可靠事实；不给摘要或系统边界虚构 Run／Step。
        // 已在源正文中的未知旧消息仍保持未知，不借一次换代推定其为本地调用。
        history.message_origins.extend(
            replacement
                .messages
                .iter()
                .map(conversation::message_id)
                .filter(|id| !previous.contains(id))
                .map(|id| StoredMessageOrigin {
                    message_id: id.clone(),
                    run_id: None,
                    input_id: None,
                    attempt: None,
                    step: None,
                    inherited: false,
                }),
        );
        history.attachments = self.history_attachment_bindings(owner, replacement)?;
        Ok(history)
    }

    /// 只在旧格式迁出时读取原关系；缺失事实不从正文猜造，关联损坏则拒绝转换。
    pub(super) fn legacy_history_facts(
        &mut self,
        owner: &ConversationOwner,
        conversation: &ConversationSnapshot,
    ) -> StorageResult<StoredHistoryFacts> {
        let keys = HistoryMessageKeys::from_conversation(conversation)?;
        self.legacy_history_facts_for_keys(owner, &keys)
    }

    pub(super) fn legacy_history_facts_for_keys(
        &mut self,
        owner: &ConversationOwner,
        keys: &HistoryMessageKeys,
    ) -> StorageResult<StoredHistoryFacts> {
        let session_id = match owner {
            ConversationOwner::MainSession { session_id }
            | ConversationOwner::ChildTask { session_id, .. } => session_id,
        };
        let ids = &keys.messages;
        let mut history = StoredHistoryFacts::default();
        let mut queued_inputs = HashSet::new();
        let mut queued_messages = HashSet::new();
        match owner {
            ConversationOwner::MainSession { .. } => {
                history.inputs = self.read_input_history(session_id, None)?;
                let raw_inputs: Vec<(String, String, String)> = self
                    .connection
                    .prepare(
                        "SELECT input_id,user_message_id,state FROM inputs WHERE session_id=?1",
                    )
                    .map_err(|e| internal_error("legacy Input identity could not be queried", e))?
                    .query_map([session_id.as_str()], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .map_err(|e| internal_error("legacy Input identity could not be read", e))?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(|e| internal_error("legacy Input identity row is invalid", e))?;
                let inputs: HashMap<_, _> = raw_inputs
                    .into_iter()
                    .map(|(input, message, state)| {
                        let input = assistant_protocol::InputId::new(input).map_err(|e| {
                            super::invalid_data_with_source("legacy Input ID is invalid", e)
                        })?;
                        let message = agent_types::MessageId::new(message).map_err(|e| {
                            super::invalid_data_with_source("legacy User Message ID is invalid", e)
                        })?;
                        if state == "queued" {
                            queued_inputs.insert(input.clone());
                            queued_messages.insert(message.clone());
                        }
                        Ok((input, message))
                    })
                    .collect::<StorageResult<_>>()?;
                for run in self.load_legacy_runs_scoped(Some(session_id))? {
                    if run.status == RunStatus::Accepted && run.message_ids.is_empty() {
                        continue;
                    }
                    if run.message_ids.iter().any(|id| !ids.contains_key(id)) {
                        return Err(invalid_data(
                            "legacy run references a missing conversation message",
                        ));
                    }
                    history
                        .message_origins
                        .extend(run.message_ids.iter().map(|id| StoredMessageOrigin {
                            message_id: id.clone(),
                            run_id: Some(run.run_id.clone()),
                            input_id: Some(run.input_id.clone()),
                            attempt: Some(run.attempt),
                            step: run.message_steps.get(id).copied(),
                            inherited: false,
                        }));
                    history.runs.push(StoredRunHistory::from_run(
                        &run,
                        inputs.get(&run.input_id).cloned(),
                    ));
                }
                history.commands = self
                    .load_session_commands_scoped(session_id)?
                    .into_iter()
                    .filter(|command| command.state == StoredSessionCommandState::Committed)
                    .collect();
                history.mcp_selections = self
                    .load_mcp_input_selections_scoped(Some(session_id))?
                    .into_iter()
                    .filter(|selection| {
                        !selection
                            .input_id
                            .as_ref()
                            .is_some_and(|id| queued_inputs.contains(id))
                            && !queued_messages.contains(&selection.message_id)
                    })
                    .collect();
                let changes: HashMap<String, i64> = self
                    .connection
                    .prepare(
                        "SELECT message_id,changed_at_ms FROM message_feedback WHERE session_id=?1",
                    )
                    .map_err(|e| internal_error("legacy message feedback could not be queried", e))?
                    .query_map([session_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map_err(|e| internal_error("legacy message feedback could not be read", e))?
                    .collect::<rusqlite::Result<_>>()
                    .map_err(|e| internal_error("legacy message feedback row is invalid", e))?;
                history.feedback = self
                    .legacy_message_feedback(session_id)?
                    .into_iter()
                    .map(|feedback| {
                        let changed_at_ms = changes
                            .get(feedback.message_id.as_str())
                            .copied()
                            .ok_or_else(|| invalid_data("legacy feedback timestamp is missing"))?;
                        Ok(StoredHistoryFeedback {
                            message_id: feedback.message_id,
                            feedback: Some(feedback.feedback),
                            changed_at_ms,
                        })
                    })
                    .collect::<StorageResult<_>>()?;
                history.questions = self
                    .load_feedback(session_id)?
                    .into_iter()
                    .filter(|request| request.status != assistant_protocol::FeedbackStatus::Pending)
                    .collect();
                history.child_tasks = self
                    .load_child_tasks_scoped(Some(session_id))?
                    .iter()
                    .map(StoredChildTaskHistory::from_task)
                    .collect();
            }
            ConversationOwner::ChildTask { child_task_id, .. } => {
                let task = self
                    .load_child_task_control(session_id, child_task_id)?
                    .ok_or_else(|| conflict("history child owner does not exist"))?;
                history
                    .child_tasks
                    .push(StoredChildTaskHistory::from_task(&task));
                history.message_origins.extend({
                    let mut ordered: Vec<_> = keys.messages.iter().collect();
                    ordered.sort_unstable_by_key(|(_, ordinal)| **ordinal);
                    ordered
                        .into_iter()
                        .map(|(message_id, _)| StoredMessageOrigin {
                            message_id: message_id.clone(),
                            run_id: None,
                            input_id: None,
                            attempt: None,
                            step: None,
                            inherited: false,
                        })
                });
            }
        }
        history.skill_activations = self
            .load_skill_activations_scoped(Some(session_id))?
            .into_iter()
            .filter(|activation| match (&activation.owner, owner) {
                (
                    assistant_runtime::SkillActivationOwner::Session(id),
                    ConversationOwner::MainSession { session_id },
                ) => id == session_id,
                (
                    assistant_runtime::SkillActivationOwner::ChildTask(id),
                    ConversationOwner::ChildTask { child_task_id, .. },
                ) => id == child_task_id.as_str(),
                _ => false,
            })
            .filter(|activation| {
                !activation
                    .input_id
                    .as_ref()
                    .is_some_and(|id| queued_inputs.contains(id))
                    && !queued_messages.contains(&activation.message_id)
            })
            .collect();
        history.attachments = self.history_attachment_paths(owner, &keys.attachment_paths)?;
        // 迁出是完整 owner 保存，不是用户裁尾。旧库已知的 child 目录即使缺少父正文
        // 也必须保留；只保留原关系和未知值，不因此补造父 Run、工具消息或开始时间。
        let children = std::mem::take(&mut history.child_tasks);
        let counts = [
            history.inputs.len(),
            history.runs.len(),
            history.message_origins.len(),
            history.commands.len(),
            history.skill_activations.len(),
            history.mcp_selections.len(),
            history.feedback.len(),
            history.questions.len(),
        ];
        retain_keyed_facts(&mut history, owner, keys);
        // 普通历史事实需要可证明的正文/Run 绑定；完整迁出不能静默套用裁尾规则丢弃旧行。
        if counts
            != [
                history.inputs.len(),
                history.runs.len(),
                history.message_origins.len(),
                history.commands.len(),
                history.skill_activations.len(),
                history.mcp_selections.len(),
                history.feedback.len(),
                history.questions.len(),
            ]
        {
            return Err(invalid_data(
                "legacy history facts have missing message or Run bindings",
            ));
        }
        history.child_tasks = children;
        history.normalize();
        Ok(history)
    }

    /// 冻结即将可靠提交的增量，必须发生在文件 fsync 和 SQLite effect 之前。
    /// 完成后的临时回执释放这些值，后续查询只读文件；此处不发布 Runtime 事件。
    pub(super) fn history_for_append(
        &mut self,
        target: &ConversationStorageTarget,
        messages: &[ConversationMessage],
        step: Option<u32>,
        purpose: &AppendPurpose,
        at_ms: i64,
        existing: Option<&StoredHistoryFacts>,
    ) -> StorageResult<StoredHistoryFacts> {
        let mut history = StoredHistoryFacts::default();
        let owner = match target {
            ConversationStorageTarget::SessionMetadata { session_id } => {
                return match purpose {
                    AppendPurpose::MessageFeedback { change } => Ok(StoredHistoryFacts {
                        feedback: vec![change.clone()],
                        ..Default::default()
                    }),
                    AppendPurpose::ChildDirectory { child, .. }
                        if !child.inherited
                            && child.status.is_some_and(|status| status.is_terminal()) =>
                    {
                        Ok(StoredHistoryFacts {
                            child_tasks: vec![(**child).clone()],
                            ..Default::default()
                        })
                    }
                    AppendPurpose::QuestionFeedback { request, .. }
                        if &request.session_id == session_id
                            && request.status != assistant_protocol::FeedbackStatus::Pending =>
                    {
                        Ok(StoredHistoryFacts {
                            questions: vec![request.clone()],
                            ..Default::default()
                        })
                    }
                    AppendPurpose::FeedbackInput { input } if &input.session_id == session_id => {
                        let request = input
                            .feedback_answer
                            .as_ref()
                            .ok_or_else(|| invalid_data("feedback Input has no answer"))?;
                        if &request.session_id != session_id
                            || request.status != assistant_protocol::FeedbackStatus::Answered
                        {
                            return Err(invalid_data("feedback Input answer is invalid"));
                        }
                        Ok(StoredHistoryFacts {
                            questions: vec![request.clone()],
                            ..Default::default()
                        })
                    }
                    _ => Err(invalid_data("session metadata purpose is invalid")),
                };
            }
            ConversationStorageTarget::Session { session_id, run_id } => {
                let run = self
                    .load_run_control(session_id, run_id)?
                    .ok_or_else(|| conflict("history Run owner does not exist"))?;
                let frozen_input = existing.and_then(|facts| {
                    facts
                        .inputs
                        .iter()
                        .find(|input| input.input_id == run.input_id)
                });
                if let Some(input) = frozen_input {
                    if input.inherited {
                        return Err(conflict(
                            "Run Input identity collides with inherited history",
                        ));
                    }
                } else {
                    history.inputs = self.read_input_history(session_id, Some(&run.input_id))?;
                }
                // 此处只读取归属及冻结 Activation，不重复解码尚未消费的 queued payload。
                // UserMessage 已由当前提交提供；读取无关队列正文会扩大历史提交的失败范围。
                let (user_message_id, activation_json): (Option<String>, Option<String>) = self.connection.query_row(
                    "SELECT user_message_id,skill_activation_json FROM inputs WHERE input_id=?1 AND session_id=?2",
                    rusqlite::params![run.input_id.as_str(), session_id.as_str()], |row| Ok((row.get(0)?,row.get(1)?))
                ).map_err(|e| internal_error("history Input identity could not be read", e))?;
                let user_message_id = user_message_id
                    .or_else(|| frozen_input.map(|input| input.user_message_id.as_str().to_owned()))
                    .map(|id| {
                        agent_types::MessageId::new(id).map_err(|e| {
                            super::invalid_data_with_source("history User Message ID is invalid", e)
                        })
                    })
                    .transpose()?;
                if frozen_input
                    .is_some_and(|input| Some(&input.user_message_id) != user_message_id.as_ref())
                {
                    return Err(conflict(
                        "Run Input identity differs from committed history",
                    ));
                }
                let activation: Option<assistant_runtime::StoredSkillActivation> = activation_json
                    .map(|json| serde_json::from_str(&json))
                    .transpose()
                    .map_err(|e| {
                        super::invalid_data_with_source("history Input Activation is invalid", e)
                    })?;
                if let Some(activation) = activation
                    && messages
                        .iter()
                        .any(|message| conversation::message_id(message) == &activation.message_id)
                {
                    history.skill_activations.push(activation);
                }
                let message_ids: HashSet<_> =
                    messages.iter().map(conversation::message_id).collect();
                history.mcp_selections = self
                    .load_mcp_input_selections_scoped(Some(session_id))?
                    .into_iter()
                    .filter(|selection| message_ids.contains(&selection.message_id))
                    .collect();
                let call_ids: HashSet<_> = messages
                    .iter()
                    .filter_map(|message| {
                        if let ConversationMessage::Assistant(assistant) = message {
                            Some(&assistant.parts)
                        } else {
                            None
                        }
                    })
                    .flatten()
                    .filter_map(|part| {
                        if let agent_types::AssistantPart::ToolCall(call) = part {
                            Some(call.id.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                if self.allow_legacy_history {
                    // 仅经备份核验的旧库升级可读取旧历史控制行。正式父帧的 child
                    // 描述只来自当前工具回执指向的可靠文件，不能扫描终态 SQL 猜关联。
                    history.child_tasks = self
                        .load_child_tasks_scoped(Some(session_id))?
                        .iter()
                        .filter(|task| {
                            &task.parent_run_id == run_id
                                && call_ids.contains(task.parent_tool_call_id.as_str())
                        })
                        .map(StoredChildTaskHistory::from_task)
                        .collect();
                } else if let AppendPurpose::ToolExchange { receipt_id, .. } = purpose {
                    let children = self.pending_child_history(receipt_id, session_id, run_id)?;
                    if children
                        .iter()
                        .any(|child| !call_ids.contains(child.parent_tool_call_id.as_str()))
                    {
                        return Err(invalid_data(
                            "pending child does not belong to committed parent calls",
                        ));
                    }
                    history.child_tasks = children;
                }
                let questions = self.load_feedback_controls(session_id)?;
                if self.allow_legacy_history {
                    history.questions = questions
                        .into_iter()
                        .filter(|question| {
                            &question.run_id == run_id
                                && question.status != assistant_protocol::FeedbackStatus::Pending
                                && call_ids.contains(question.tool_call_id.as_str())
                        })
                        .collect();
                } else if let AppendPurpose::RunSettlement { status, .. } = purpose {
                    history.questions = questions
                        .into_iter()
                        .filter(|question| {
                            &question.run_id == run_id
                                && question.status == assistant_protocol::FeedbackStatus::Pending
                                && (question.mode == assistant_protocol::FeedbackMode::Wait
                                    || *status != RunStatus::Completed)
                        })
                        .map(|mut question| {
                            question.status = assistant_protocol::FeedbackStatus::Invalidated;
                            question
                        })
                        .collect();
                }
                let mut fact = StoredRunHistory::from_run(&run, user_message_id);
                match purpose {
                    AppendPurpose::UserMessage {
                        reasoning_effort,
                        shell,
                        ..
                    } => {
                        fact.status = Some(RunStatus::Running);
                        fact.started_at_ms = Some(at_ms);
                        fact.reasoning_effort = *reasoning_effort;
                        fact.agent_shell_kind = shell.as_ref().map(|shell| shell.kind);
                    }
                    AppendPurpose::RunSettlement {
                        status,
                        cancel_requested,
                        error,
                        ..
                    } => {
                        fact.status = Some(*status);
                        fact.finished_at_ms = Some(at_ms);
                        fact.cancel_requested = *cancel_requested;
                        fact.error = error.clone();
                        if !self.allow_legacy_history {
                            fact.finished_after_message_id = if let Some(last) = messages.last() {
                                Some(conversation::message_id(last).clone())
                            } else {
                                let generation = self.session_generation(session_id)?;
                                let path =
                                    body_path(&self.session_directory(session_id)?, generation);
                                self.conversation_indexes.last_message_id(
                                    &path,
                                    &target.owner(),
                                    generation,
                                )?
                            };
                        }
                    }
                    _ => {}
                }
                if let Some(last) = messages.last() {
                    fact.last_message_id = Some(conversation::message_id(last).clone());
                }
                history.runs.push(fact);
                history
                    .message_origins
                    .extend(messages.iter().map(|message| StoredMessageOrigin {
                        message_id: conversation::message_id(message).clone(),
                        run_id: Some(run_id.clone()),
                        input_id: Some(run.input_id.clone()),
                        attempt: Some(run.attempt),
                        step,
                        inherited: false,
                    }));
                if run.attempt == 1
                    && let Some(user) = history
                        .runs
                        .last()
                        .and_then(|fact| fact.user_message_id.as_ref())
                    && run.message_ids.contains(user)
                    && !messages
                        .iter()
                        .any(|message| conversation::message_id(message) == user)
                    && existing.is_none_or(|facts| {
                        !facts
                            .message_origins
                            .iter()
                            .any(|origin| &origin.message_id == user)
                    })
                {
                    history.message_origins.push(StoredMessageOrigin {
                        message_id: user.clone(),
                        run_id: Some(run.run_id.clone()),
                        input_id: Some(run.input_id.clone()),
                        attempt: Some(run.attempt),
                        step: run.message_steps.get(user).copied(),
                        inherited: false,
                    });
                }
                ConversationOwner::MainSession {
                    session_id: session_id.clone(),
                }
            }
            ConversationStorageTarget::ChildTask {
                session_id,
                child_task_id,
            } => {
                let task = self
                    .load_child_task_control(session_id, child_task_id)?
                    .ok_or_else(|| conflict("history child owner does not exist"))?;
                let mut fact = StoredChildTaskHistory::from_task(&task);
                fact.message_count = fact
                    .message_count
                    .checked_add(messages.len() as u64)
                    .ok_or_else(|| invalid_data("child history message count overflows"))?;
                match purpose {
                    AppendPurpose::ChildStart => {
                        fact.status = Some(assistant_protocol::ChildTaskStatus::Running);
                        fact.started_at_ms = Some(at_ms);
                    }
                    AppendPurpose::ChildSettlement {
                        status,
                        error,
                        final_message_id,
                        cancel_requested,
                        ..
                    } => {
                        fact.status = Some(*status);
                        fact.finished_at_ms = Some(at_ms);
                        fact.error = error.clone();
                        fact.final_message_id = final_message_id.clone();
                        fact.cancel_requested |= *cancel_requested;
                    }
                    _ => {}
                }
                history.child_tasks.push(fact);
                history
                    .message_origins
                    .extend(messages.iter().map(|message| StoredMessageOrigin {
                        message_id: conversation::message_id(message).clone(),
                        run_id: None,
                        input_id: None,
                        attempt: None,
                        step,
                        inherited: false,
                    }));
                ConversationOwner::ChildTask {
                    session_id: session_id.clone(),
                    child_task_id: child_task_id.clone(),
                }
            }
        };
        match purpose {
            AppendPurpose::ToolExchange {
                skill_activations, ..
            }
            | AppendPurpose::ChildToolExchange {
                skill_activations, ..
            } => history
                .skill_activations
                .extend(skill_activations.iter().cloned()),
            _ => {}
        }
        if let Some(existing) = existing
            && (history.skill_activations.iter().any(|next| {
                existing
                    .skill_activations
                    .iter()
                    .any(|previous| previous.activation_id == next.activation_id)
            }) || history.mcp_selections.iter().any(|next| {
                existing
                    .mcp_selections
                    .iter()
                    .any(|previous| previous.selection_id == next.selection_id)
            }))
        {
            return Err(conflict(
                "history selection identity is already committed in this owner",
            ));
        }
        history.attachments = self
            .history_attachment_bindings(&owner, &ConversationSnapshot::new(messages.to_vec()))?;
        Ok(history)
    }

    fn history_attachment_bindings(
        &self,
        owner: &ConversationOwner,
        conversation: &ConversationSnapshot,
    ) -> StorageResult<Vec<StoredHistoryAttachment>> {
        let references: HashSet<_> = conversation
            .messages
            .iter()
            .filter_map(|message| match message {
                ConversationMessage::User(user) => Some(&user.parts),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                UserPart::FileReferences(files) => Some(&files.files),
                _ => None,
            })
            .flatten()
            .map(|file| file.readable_path.clone())
            .collect();
        self.history_attachment_paths(owner, &references)
    }

    fn history_attachment_paths(
        &self,
        owner: &ConversationOwner,
        references: &HashSet<String>,
    ) -> StorageResult<Vec<StoredHistoryAttachment>> {
        let session_id = match owner {
            ConversationOwner::MainSession { session_id }
            | ConversationOwner::ChildTask { session_id, .. } => session_id,
        };
        let directory = self.session_directory(session_id)?;
        self.load_attachments_scoped(Some(session_id))?
            .into_iter()
            .filter(|attachment| {
                references.contains(attachment.agent_readable_path.as_str())
                    || super::resource_reference::historical_attachment_path(
                        &self.runtime_home,
                        attachment,
                    )
                    .as_deref()
                    .is_some_and(|path| references.contains(path))
            })
            .map(|attachment| {
                let relative = std::path::Path::new(&attachment.agent_readable_path)
                    .strip_prefix(&directory)
                    .map_err(|_| invalid_data("history attachment escapes its managed owner"))?;
                Ok(StoredHistoryAttachment {
                    attachment_id: attachment.attachment_id,
                    blob_hash: attachment.blob_hash,
                    original_name: attachment.original_name,
                    size_bytes: attachment.size_bytes,
                    media_type: attachment.media_type,
                    relative_path: relative
                        .to_str()
                        .ok_or_else(|| invalid_data("history attachment path is not UTF-8"))?
                        .replace('\\', "/"),
                })
            })
            .collect()
    }
}

/// 只供同 owner 的完整换代或历史重入裁尾；Fork 的物理提交截点另行核对。
fn retain_replacement_facts(
    history: &mut StoredHistoryFacts,
    owner: &ConversationOwner,
    replacement: &ConversationSnapshot,
) -> StorageResult<()> {
    // 调用方已验证规范消息；此处只提取定位键供原有保留规则使用。
    let keys = HistoryMessageKeys::from_conversation(replacement)?;
    retain_keyed_facts(history, owner, &keys);
    Ok(())
}

fn retain_keyed_facts(
    history: &mut StoredHistoryFacts,
    owner: &ConversationOwner,
    keys: &HistoryMessageKeys,
) {
    let ids = &keys.messages;
    let mut retained_runs = HashSet::new();
    for origin in &history.message_origins {
        if ids.contains_key(&origin.message_id)
            && let Some(run_id) = &origin.run_id
        {
            retained_runs.insert(run_id.clone());
        }
    }
    let incomplete: HashSet<_> = history
        .message_origins
        .iter()
        .filter(|origin| !ids.contains_key(&origin.message_id))
        .filter_map(|origin| origin.run_id.clone())
        .collect();
    history.runs.retain_mut(|run| {
        let lost_end = run
            .finished_after_message_id
            .as_ref()
            .is_some_and(|id| !ids.contains_key(id));
        if lost_end && !retained_runs.contains(&run.run_id) {
            return false;
        }
        let retained = retained_runs.contains(&run.run_id)
            || run
                .user_message_id
                .as_ref()
                .is_some_and(|id| ids.contains_key(id));
        if retained && (incomplete.contains(&run.run_id) || lost_end) {
            run.partial = true;
            run.status = None;
            run.error = None;
            run.finished_at_ms = None;
            run.finished_after_message_id = None;
        }
        retained
    });
    history
        .message_origins
        .retain(|origin| ids.contains_key(&origin.message_id));
    retained_runs.extend(history.runs.iter().map(|run| run.run_id.clone()));
    let retained_inputs: HashSet<_> = history
        .runs
        .iter()
        .filter_map(|run| run.input_id.as_ref())
        .collect();
    history.inputs.retain(|input| {
        ids.contains_key(&input.user_message_id) || retained_inputs.contains(&input.input_id)
    });
    for run in &mut history.runs {
        if run
            .last_message_id
            .as_ref()
            .is_some_and(|id| !ids.contains_key(id))
        {
            run.last_message_id = history
                .message_origins
                .iter()
                .rev()
                .find(|origin| origin.run_id.as_ref() == Some(&run.run_id))
                .map(|origin| origin.message_id.clone());
        }
    }
    let call_ids = &keys.calls;
    history.child_tasks.retain(|child| match owner {
        ConversationOwner::MainSession { .. } => {
            retained_runs.contains(&child.parent_run_id)
                && call_ids.contains(child.parent_tool_call_id.as_str())
        }
        ConversationOwner::ChildTask { child_task_id, .. } => child.child_task_id == *child_task_id,
    });
    history
        .commands
        .retain(|command| ids.contains_key(&command.user_message_id));
    history
        .skill_activations
        .retain(|activation| ids.contains_key(&activation.message_id));
    history
        .mcp_selections
        .retain(|selection| ids.contains_key(&selection.message_id));
    history.feedback.retain(|feedback| {
        ids.keys()
            .any(|id| id.as_str() == feedback.message_id.as_str())
    });
    history
        .questions
        .retain(|question| retained_runs.contains(&question.run_id));
}
