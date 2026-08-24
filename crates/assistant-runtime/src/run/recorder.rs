//! Core 两阶段 Recorder 到主 Run 或子任务独立 Journal 的统一绑定。

mod target;

use std::sync::Arc;

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{AssistantMessage, ToolCallId, ToolMessage};
use assistant_protocol::{RunId, ToolCallId as ProtocolToolCallId};

use crate::{
    RuntimeStore, delegation::ChildTaskRecord as ChildTaskRecordImpl, id,
    session::SessionController,
};

use self::target::RecorderTarget;

/// 把 Core 的两阶段落账调用绑定到唯一的规范 Conversation 目标。
pub(crate) struct RuntimeRecorder {
    target: RecorderTarget,
    store: Arc<dyn RuntimeStore>,
}

impl RuntimeRecorder {
    pub(crate) fn new(
        session: Arc<SessionController>,
        run_id: RunId,
        store: Arc<dyn RuntimeStore>,
    ) -> Self {
        Self {
            target: RecorderTarget::parent(session, run_id),
            store,
        }
    }

    pub(crate) fn for_child(task: Arc<ChildTaskRecordImpl>, store: Arc<dyn RuntimeStore>) -> Self {
        Self {
            target: RecorderTarget::child(task),
            store,
        }
    }
}

impl ExecutionRecorder for RuntimeRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
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
                .commit_begin(receipt.clone(), assistant)
                .inspect_err(|_| self.target.fault())?;
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
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let _mutation = self.target.mutation().await;
            let batch = self
                .target
                .validate_complete(receipt, &results)
                .inspect_err(|_| self.target.fault())?;
            let operation_id = id::generate("append")
                .map_err(|_| record_error("storage operation id could not be allocated"))?;
            let completed_at_ms = crate::runtime::now_ms()
                .map_err(|_| record_error("tool exchange time could not be recorded"))?;
            if self
                .target
                .persist_complete(
                    self.store.as_ref(),
                    operation_id,
                    receipt.clone(),
                    results.clone(),
                    completed_at_ms,
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
                .commit_complete(receipt, results, &batch)
                .inspect_err(|_| self.target.fault())
        })
    }
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
                title: "recorder fixture".to_owned(),
                title_origin: assistant_protocol::SessionTitleOrigin::Generated,
                model_key: ModelKey::new("fixture").expect("model key"),
                reasoning_effort: None,
                system_prompt: SystemPromptSnapshot::new(vec!["parent".to_owned()]),
                environment: SessionExecutionEnvironment {
                    workspace_id: None,
                    working_directory: "/volatile/session/private".to_owned(),
                    workspace_private_directory: None,
                    session_attachment_directory: "/volatile/session/attachments".to_owned(),
                    session_tool_image_directory: "/volatile/session/tool-images".to_owned(),
                    session_private_directory: "/volatile/session/private".to_owned(),
                },
                current_variant: AgentVariant::Build,
                approval_mode: ApprovalMode::Ask,
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
        let stored = store
            .load_runtime()
            .await
            .expect("load child projection")
            .child_tasks
            .into_iter()
            .find(|task| task.child_task_id == child_task_id)
            .expect("stored child task");
        let task = Arc::new(
            ChildTaskRecord::recovered(&stored, Some(conversation)).expect("recover child record"),
        );
        let recorder = RuntimeRecorder::for_child(task, store.clone());
        let call_id = ToolCallId::new("child-call").expect("call id");
        let receipt = recorder
            .begin_tool_exchange(AssistantMessage {
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
            })
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
