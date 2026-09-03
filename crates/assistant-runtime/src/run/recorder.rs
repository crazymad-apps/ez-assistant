//! Core 两阶段 Recorder 到主 Run 或子任务独立 Journal 的统一绑定。

mod target;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use agent_core::{
    ExchangeCompletion, ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture,
};
use agent_types::{AssistantMessage, ToolCallId, ToolMessage};
use assistant_protocol::{RunId, ToolCallId as ProtocolToolCallId};

use crate::{
    RuntimeStore,
    delegation::ChildTaskRecord as ChildTaskRecordImpl,
    id,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    observation::ObservationCoordinator,
    session::SessionController,
    skill::{
        SkillActivationLatch, SkillActivationTrigger, StoredSkillActivation,
        render_model_activation,
    },
};

use self::target::{PersistedToolExchangeCompletion, RecorderTarget};

/// 一批成功 `load_skill` 调用生成的冻结持久化事实。
struct PreparedSkillActivations {
    message: Option<agent_types::UserMessage>,
    activations: Vec<StoredSkillActivation>,
    call_ids: Vec<ToolCallId>,
}

/// 把 Core 的两阶段落账调用绑定到唯一的规范 Conversation 目标。
pub(crate) struct RuntimeRecorder {
    target: RecorderTarget,
    store: Arc<dyn RuntimeStore>,
    pending_steps: Mutex<HashMap<String, u32>>,
    events: ObservationCoordinator,
    skill_activation_latch: Arc<SkillActivationLatch>,
}

impl RuntimeRecorder {
    pub(crate) fn new(
        session: Arc<SessionController>,
        run_id: RunId,
        store: Arc<dyn RuntimeStore>,
        events: ObservationCoordinator,
        skill_activation_latch: Arc<SkillActivationLatch>,
    ) -> Self {
        Self {
            target: RecorderTarget::parent(session, run_id),
            store,
            pending_steps: Mutex::new(HashMap::new()),
            events,
            skill_activation_latch,
        }
    }

    pub(crate) fn for_child(
        task: Arc<ChildTaskRecordImpl>,
        session: Arc<SessionController>,
        store: Arc<dyn RuntimeStore>,
        events: ObservationCoordinator,
        skill_activation_latch: Arc<SkillActivationLatch>,
    ) -> Self {
        Self {
            target: RecorderTarget::child(task, session),
            store,
            pending_steps: Mutex::new(HashMap::new()),
            events,
            skill_activation_latch,
        }
    }
}

impl ExecutionRecorder for RuntimeRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        step: u32,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            // 各目标独占自己的 mutation gate；Store await 不能被同一目标的终态结算越过，
            // sibling child 之间则不互相阻塞。
            let _mutation = self.target.mutation().await;
            self.target
                .validate_begin(&assistant)
                .inspect_err(|_| self.target.fault())?;

            let receipt = ExchangeReceipt::new(
                id::generate("exchange")
                    .map_err(|_| record_error("tool exchange id could not be allocated"))?,
            )?;
            let created_at_ms = crate::runtime::now_ms()
                .map_err(|_| record_error("tool exchange time could not be recorded"))?;
            if self
                .target
                .persist_begin(
                    self.store.as_ref(),
                    receipt.clone(),
                    step,
                    assistant.clone(),
                    created_at_ms,
                )
                .await
                .is_err()
            {
                self.target.fault();
                return Err(record_error("tool exchange begin could not be persisted"));
            }
            self.target
                .commit_begin(receipt.clone(), step, assistant)
                .inspect_err(|_| self.target.fault())?;
            self.pending_steps
                .lock()
                .map_err(|_| record_error("tool exchange step registry is unavailable"))?
                .insert(receipt.as_str().to_owned(), step);
            Ok(receipt)
        })
    }

    fn mark_tool_execution_started<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        call_id: &'a ToolCallId,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let call_id = ProtocolToolCallId::new(call_id.as_str())
                .map_err(|_| record_error("tool call id could not be recorded"))?;
            let started_at_ms = crate::runtime::now_ms()
                .map_err(|_| record_error("tool execution start time could not be recorded"))?;
            self.target
                .persist_started(self.store.as_ref(), receipt.clone(), call_id, started_at_ms)
                .await
                .map_err(|_| record_error("tool execution start could not be persisted"))
        })
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ExchangeCompletion> {
        Box::pin(async move {
            let _mutation = self.target.mutation().await;
            let completed_at_ms = crate::runtime::now_ms()
                .map_err(|_| record_error("tool exchange time could not be recorded"))?;
            let staged = self
                .skill_activation_latch
                .staged_for_results(&results)
                .map_err(|_| record_error("skill activation latch is unavailable"))?;
            let prepared = prepare_skill_activations(&self.target, staged, completed_at_ms)?;
            let batch = self
                .target
                .validate_complete(receipt, &results, prepared.message.as_ref())
                .inspect_err(|_| self.target.fault())?;
            let step = self
                .pending_steps
                .lock()
                .map_err(|_| record_error("tool exchange step registry is unavailable"))?
                .get(receipt.as_str())
                .copied()
                .ok_or_else(|| record_error("tool exchange step is unavailable"))?;
            let operation_id = id::generate("append")
                .map_err(|_| record_error("storage operation id could not be allocated"))?;
            if self
                .target
                .persist_complete(
                    self.store.as_ref(),
                    PersistedToolExchangeCompletion {
                        operation_id,
                        receipt: receipt.clone(),
                        step,
                        results: results.clone(),
                        activation_message: prepared.message.clone(),
                        skill_activations: prepared.activations.clone(),
                        completed_at_ms,
                    },
                )
                .await
                .is_err()
            {
                self.target.fault();
                return Err(record_error(
                    "tool exchange completion could not be persisted",
                ));
            }
            self.target
                .commit_complete(
                    receipt,
                    step,
                    results,
                    prepared.message,
                    prepared.activations,
                    &batch,
                )
                .inspect_err(|_| self.target.fault())?;
            self.skill_activation_latch
                .commit(&prepared.call_ids)
                .map_err(|_| record_error("skill activation latch could not be committed"))?;
            let (owner, generation) = self.target.committed_projection()?;
            let _ = self
                .events
                .send(assistant_protocol::RuntimeEvent::ConversationCommitted {
                    owner: owner.clone(),
                    generation,
                });
            let _ = self
                .events
                .send(assistant_protocol::RuntimeEvent::StepCommitted {
                    owner,
                    step,
                    generation,
                });
            self.pending_steps
                .lock()
                .map_err(|_| record_error("tool exchange step registry is unavailable"))?
                .remove(receipt.as_str());
            Ok(ExchangeCompletion {
                continuation_required: !prepared.call_ids.is_empty(),
            })
        })
    }
}

fn prepare_skill_activations(
    target: &RecorderTarget,
    staged: Vec<(ToolCallId, crate::SessionSkillDefinition)>,
    created_at_ms: i64,
) -> Result<PreparedSkillActivations, RecordError> {
    if staged.is_empty() {
        return Ok(PreparedSkillActivations {
            message: None,
            activations: Vec::new(),
            call_ids: Vec::new(),
        });
    }
    let (session_id, owner, run_id, catalog_revision) = target.skill_activation_context();
    let first = &staged[0].1;
    let (mut message, _) = InternalBoundaryCoordinator::hidden_message(InternalBoundaryRequest {
        source: InternalBoundarySource::SkillActivation,
        text: render_model_activation(&catalog_revision, first),
    })
    .map_err(|_| record_error("skill activation boundary could not be constructed"))?;
    for (_, definition) in staged.iter().skip(1) {
        InternalBoundaryCoordinator::append(
            &mut message,
            InternalBoundaryRequest {
                source: InternalBoundarySource::SkillActivation,
                text: render_model_activation(&catalog_revision, definition),
            },
        )
        .map_err(|_| record_error("skill activation boundary could not be constructed"))?;
    }
    let activations = staged
        .iter()
        .map(|(_, definition)| {
            Ok(StoredSkillActivation {
                activation_id: id::generate("activation")
                    .map_err(|_| record_error("skill activation id could not be allocated"))?,
                session_id: session_id.clone(),
                owner: owner.clone(),
                run_id: Some(run_id.clone()),
                input_id: None,
                message_id: message.id.clone(),
                name: definition.name.clone(),
                catalog_revision: catalog_revision.clone(),
                definition_digest: definition.definition_digest.clone(),
                trigger: SkillActivationTrigger::Model,
                created_at_ms,
            })
        })
        .collect::<Result<Vec<_>, RecordError>>()?;
    let call_ids = staged.into_iter().map(|(call_id, _)| call_id).collect();
    Ok(PreparedSkillActivations {
        message: Some(message),
        activations,
        call_ids,
    })
}

pub(super) fn record_error(message: &'static str) -> RecordError {
    RecordError {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_model::SystemPromptSnapshot;
    use agent_types::{
        AssistantPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId, TextPart,
        ToolCall, ToolName, ToolResult, ToolResultContent, ToolResultStatus, UserMessage, UserPart,
    };
    use assistant_protocol::{
        AgentVariant, ApprovalMode, ChildTaskId, InputId, ModelKey, RunId, SessionId,
    };
    use serde_json::json;

    use crate::{
        ChildTaskStart, NewStoredChildTask, NewStoredInput, NewStoredSession,
        SessionExecutionEnvironment, delegation::ChildTaskRecord, storage::VolatileRuntimeStore,
    };

    #[tokio::test]
    async fn child_target_uses_the_same_recorder_algorithm_with_an_independent_journal() {
        let store = Arc::new(VolatileRuntimeStore::default());
        let session_id = SessionId::new("s-recorder-child").expect("session id");
        let run_id = RunId::new("r-recorder-child").expect("run id");
        store
            .create_session(NewStoredSession {
                session_id: session_id.clone(),
                materialization_key: None,
                title: "recorder fixture".to_owned(),
                title_origin: assistant_protocol::SessionTitleOrigin::Generated,
                automatic_title_pending: false,
                model_key: ModelKey::new("fixture").expect("model key"),
                reasoning_effort: None,
                system_prompt: SystemPromptSnapshot::new(vec!["parent".to_owned()]),
                skill_catalog: crate::SessionSkillCatalog::legacy_unavailable(),
                environment: SessionExecutionEnvironment {
                    workspace_id: None,
                    working_directory: "/volatile/session/private".to_owned(),
                    additional_workspace_directories: Vec::new(),
                    workspace_private_directory: None,
                    session_attachment_directory: "/volatile/session/attachments".to_owned(),
                    session_tool_image_directory: "/volatile/session/tool-images".to_owned(),
                    session_private_directory: "/volatile/session/private".to_owned(),
                },
                current_variant: AgentVariant::Build,
                approval_mode: ApprovalMode::Ask,
                role: crate::SessionRole::Standard,
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        store
            .accept_input(NewStoredInput {
                input_id: InputId::new("input-recorder-child").expect("input id"),
                run_id: run_id.clone(),
                session_id: session_id.clone(),
                idempotency_key: None,
                agent_variant: AgentVariant::Build,
                origin: crate::InputOrigin::User,
                goal_binding: None,
                cross_session: None,
                channel_source: Some(crate::InputChannelSource::desktop_text()),
                skill_activation: None,
                mcp_selection: None,
                approval_mode: ApprovalMode::Ask,
                message: user_message("parent-user"),
                new_goal: None,
                resumed_goal: None,
                generated_title: None,
                accepted_at_ms: 2,
            })
            .await
            .expect("create parent run");
        let child_task_id = ChildTaskId::new("ct-recorder-child").expect("child id");
        store
            .create_child_task(NewStoredChildTask {
                child_task_id: child_task_id.clone(),
                session_id: session_id.clone(),
                parent_run_id: run_id,
                parent_tool_call_id: ProtocolToolCallId::new("delegate-call")
                    .expect("parent call id"),
                title: "child".to_owned(),
                system_prompt: SystemPromptSnapshot::new(vec!["child".to_owned()]),
                agent_variant: AgentVariant::Build,
                created_at_ms: 3,
            })
            .await
            .expect("create child");
        store
            .start_child_task(ChildTaskStart {
                operation_id: "start-child".to_owned(),
                child_task_id: child_task_id.clone(),
                session_id: session_id.clone(),
                message: user_message("child-user"),
                started_at_ms: 4,
            })
            .await
            .expect("start child");
        let conversation = store
            .load_child_conversation(&session_id, &child_task_id)
            .await
            .expect("load child conversation");
        let recovered = store.load_runtime().await.expect("load child projection");
        let session = Arc::new(SessionController::new(
            recovered
                .sessions
                .into_iter()
                .find(|session| session.session_id == session_id)
                .expect("stored parent session"),
        ));
        let stored = recovered
            .child_tasks
            .into_iter()
            .find(|task| task.child_task_id == child_task_id)
            .expect("stored child task");
        let task = Arc::new(
            ChildTaskRecord::recovered(&stored, Some(conversation)).expect("recover child record"),
        );
        let recorder = RuntimeRecorder::for_child(
            task,
            session,
            store.clone(),
            crate::observation::ObservationCoordinator::new(16),
            Arc::new(crate::skill::SkillActivationLatch::new(Vec::new())),
        );
        let call_id = ToolCallId::new("child-call").expect("call id");
        let receipt = recorder
            .begin_tool_exchange(
                1,
                AssistantMessage {
                    id: MessageId::new("child-assistant-tool").expect("message id"),
                    model: ModelIdentity::new(
                        ProviderId::new("fixture").expect("provider id"),
                        "fixture",
                    ),
                    parts: vec![AssistantPart::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: ToolName::new("fixture").expect("tool name"),
                        arguments: json!({}),
                    })],
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            )
            .await
            .expect("begin child exchange");
        recorder
            .mark_tool_execution_started(&receipt, &call_id)
            .await
            .expect("mark child tool started");
        recorder
            .complete_tool_exchange(
                &receipt,
                vec![ToolMessage {
                    id: MessageId::new("child-tool-result").expect("message id"),
                    result: ToolResult {
                        call_id,
                        status: ToolResultStatus::Success,
                        content: ToolResultContent::text("done".to_owned()),
                        metadata: None,
                    },
                }],
            )
            .await
            .expect("complete child exchange");

        assert_eq!(
            store
                .load_child_conversation(&session_id, &child_task_id)
                .await
                .expect("load persisted child conversation")
                .messages
                .len(),
            3
        );
        assert!(
            store
                .load_conversation(&session_id)
                .await
                .expect("load parent conversation")
                .messages
                .is_empty()
        );
    }

    fn user_message(id: &str) -> UserMessage {
        UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new(id).expect("message id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(format!("part-{id}")).expect("part id"),
                text: id.to_owned(),
            })],
        }
    }
}
