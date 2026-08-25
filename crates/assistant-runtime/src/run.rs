//! Runtime Run 领域状态、标识分配与子模块入口。

mod model_diagnostics;
mod recorder;
mod settlement;
mod supervisor;

use std::{collections::HashMap, sync::Arc};

pub(crate) use model_diagnostics::{ModelFailureDiagnostics, RunModelDiagnostics};
pub(crate) use recorder::RuntimeRecorder;
pub(crate) use settlement::{RunSettlementResult, settle_run, settle_run_with_error};
pub(crate) use supervisor::observe_run_execution;

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, FileReference, FileReferencesPart,
    MessageId, PartId, TextPart, UserMessage, UserPart,
};
use assistant_protocol::{
    AgentVariant, ApprovalMode, InputId, ReasoningEffortKey, RunId, RunSnapshot, RunStatus,
    RuntimeErrorInfo, RuntimeEvent, SessionId, ToolActivitySnapshot,
};
use tokio_util::sync::CancellationToken;

use crate::{
    RuntimeError, RuntimeResult, StoredRun, id,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    session::SessionState,
};

use self::settlement::RunSettlement;

/// Session 内保存的 Runtime Run 权威记录。
pub(crate) struct RunRecord {
    run_id: RunId,
    session_id: SessionId,
    input_id: InputId,
    attempt: u32,
    created_at_ms: Option<i64>,
    status: RunStatus,
    variant: AgentVariant,
    approval_mode: ApprovalMode,
    reasoning_effort: Option<ReasoningEffortKey>,
    cancel_requested: bool,
    active_step: Option<u32>,
    reasoning: String,
    text: String,
    tools: Vec<ToolActivitySnapshot>,
    error: Option<RuntimeErrorInfo>,
    message_ids: Vec<MessageId>,
    message_steps: HashMap<MessageId, u32>,
    finished_at_ms: Option<i64>,
}

impl RunRecord {
    pub(crate) fn accepted(run: &StoredRun, message_ids: Vec<MessageId>) -> Self {
        Self {
            run_id: run.run_id.clone(),
            session_id: run.session_id.clone(),
            input_id: run.input_id.clone(),
            attempt: run.attempt,
            created_at_ms: Some(run.created_at_ms),
            status: RunStatus::Accepted,
            variant: run.agent_variant,
            approval_mode: run.approval_mode,
            reasoning_effort: run.reasoning_effort,
            cancel_requested: false,
            active_step: None,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
            message_ids,
            message_steps: run.message_steps.clone(),
            finished_at_ms: None,
        }
    }

    pub(crate) fn recovered(run: StoredRun) -> Self {
        Self {
            run_id: run.run_id,
            session_id: run.session_id,
            input_id: run.input_id,
            attempt: run.attempt,
            created_at_ms: Some(run.created_at_ms),
            status: run.status,
            variant: run.agent_variant,
            approval_mode: run.approval_mode,
            reasoning_effort: run.reasoning_effort,
            cancel_requested: run.cancel_requested,
            active_step: None,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: run.error,
            message_ids: run.message_ids,
            message_steps: run.message_steps,
            finished_at_ms: run.finished_at_ms,
        }
    }

    /// 从规范正文恢复可派生的终态文本；流式观察字段不单独持久化。
    pub(crate) fn hydrate(&mut self, conversation: &ConversationSnapshot) {
        let final_assistant = conversation.messages.iter().rev().find_map(|message| {
            let ConversationMessage::Assistant(message) = message else {
                return None;
            };
            self.message_ids
                .iter()
                .any(|message_id| message_id == &message.id)
                .then_some(message)
        });
        let Some(message) = final_assistant else {
            return;
        };
        self.reasoning.clear();
        self.text.clear();
        for part in &message.parts {
            match part {
                AssistantPart::Reasoning(part) => self.reasoning.push_str(&part.text),
                AssistantPart::Text(part) => self.text.push_str(&part.text),
                AssistantPart::ToolCall(_) | AssistantPart::ProviderState(_) => {}
            }
        }
    }

    pub(crate) fn extend_message_ids(&mut self, messages: impl IntoIterator<Item = MessageId>) {
        self.message_ids.extend(messages);
    }

    pub(crate) fn extend_message_ids_at_step(
        &mut self,
        messages: impl IntoIterator<Item = MessageId>,
        step: u32,
    ) {
        for message_id in messages {
            self.message_steps.insert(message_id.clone(), step);
            self.message_ids.push(message_id);
        }
    }

    pub(crate) fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            input_id: self.input_id.clone(),
            attempt: self.attempt,
            created_at_ms: self.created_at_ms,
            finished_at_ms: self.finished_at_ms,
            status: self.status,
            variant: self.variant,
            approval_mode: self.approval_mode,
            reasoning_effort: self.reasoning_effort,
            cancel_requested: self.cancel_requested,
            active_step: self.active_step,
            reasoning: self.reasoning.clone(),
            text: self.text.clone(),
            tools: self.tools.clone(),
            error: self.error.clone(),
        }
    }

    pub(crate) fn mark_running(&mut self) -> bool {
        if self.status == RunStatus::Accepted {
            self.status = RunStatus::Running;
            true
        } else {
            false
        }
    }

    pub(crate) fn freeze_reasoning_effort(&mut self, effort: Option<ReasoningEffortKey>) {
        debug_assert_eq!(self.status, RunStatus::Accepted);
        self.reasoning_effort = effort;
    }

    pub(crate) fn mark_cancelling(&mut self) -> bool {
        if self.status.is_terminal() || self.status == RunStatus::Cancelling {
            return false;
        }
        self.status = RunStatus::Cancelling;
        self.cancel_requested = true;
        true
    }

    /// 开始新的模型 Step 时只保留该 Step 的流式可见正文；可靠历史由 Conversation 承载。
    pub(crate) fn start_step(&mut self, step: u32) {
        if self.active_step == Some(step) {
            return;
        }
        self.active_step = Some(step);
        self.reasoning.clear();
        self.text.clear();
    }

    /// 即使 `StepStarted` 被背压丢弃，delta 自身也能建立正确 step 边界。
    pub(crate) fn append_reasoning(&mut self, step: u32, delta: &str) {
        self.start_step(step);
        self.reasoning.push_str(delta);
    }

    /// 即使 `StepStarted` 被背压丢弃，delta 自身也能建立正确 step 边界。
    pub(crate) fn append_text(&mut self, step: u32, delta: &str) {
        self.start_step(step);
        self.text.push_str(delta);
    }

    pub(crate) fn input_id(&self) -> &InputId {
        &self.input_id
    }
    pub(crate) fn message_ids(&self) -> &[MessageId] {
        &self.message_ids
    }
    pub(crate) fn message_step(&self, message_id: &MessageId) -> Option<u32> {
        self.message_steps.get(message_id).copied()
    }
    pub(crate) fn attempt(&self) -> u32 {
        self.attempt
    }
    pub(crate) fn variant(&self) -> AgentVariant {
        self.variant
    }
    pub(crate) fn approval_mode(&self) -> ApprovalMode {
        self.approval_mode
    }
    pub(crate) fn status(&self) -> RunStatus {
        self.status
    }
    pub(crate) fn finished_at_ms(&self) -> Option<i64> {
        self.finished_at_ms
    }
    pub(crate) fn fail_before_start(&mut self, error: RuntimeErrorInfo, finished_at_ms: i64) {
        self.status = RunStatus::Failed;
        self.error = Some(error);
        self.finished_at_ms = Some(finished_at_ms);
    }

    fn settle(&mut self, settlement: RunSettlement, finished_at_ms: i64) {
        self.status = settlement.status;
        if let Some(reasoning) = settlement.reasoning {
            self.reasoning = reasoning;
        }
        if let Some(text) = settlement.text {
            self.text = text;
        }
        self.error = settlement.error;
        self.finished_at_ms = Some(finished_at_ms);
    }
}

/// 当前活动 Run 的执行控制句柄；Session 终态后立即清除。
pub(crate) struct ActiveRun {
    pub(crate) run_id: RunId,
    pub(crate) cancellation: CancellationToken,
    pub(crate) goal_signal_latch: Option<Arc<crate::goal::GoalRunSignalLatch>>,
}

/// 从已结算快照投影 Runtime 的唯一 RunFinished 事件。
pub(crate) fn finished_event(snapshot: RunSnapshot) -> RuntimeEvent {
    RuntimeEvent::RunFinished {
        session_id: snapshot.session_id,
        run_id: snapshot.run_id,
        status: snapshot.status,
        error: snapshot.error,
    }
}

/// 在当前 Session 的 Run Registry 中分配一个未占用标识。
pub(crate) fn allocate_run_id(state: &SessionState) -> RuntimeResult<RunId> {
    for _ in 0..id::GENERATION_ATTEMPTS {
        let value = id::generate("r").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "run id random source",
        })?;
        let id = RunId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "run id generator",
        })?;
        if !state.runs.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "run id collision",
    })
}

/// 将已校验文本和已冻结文件引用封装为新的规范 UserMessage。
pub(crate) fn create_user_message(
    text: String,
    files: Vec<FileReference>,
    variant: AgentVariant,
) -> RuntimeResult<UserMessage> {
    let message_id = allocate_message_id()?;
    let mut parts = vec![UserPart::Text(TextPart {
        id: allocate_part_id()?,
        text,
    })];
    if !files.is_empty() {
        parts.push(UserPart::FileReferences(FileReferencesPart {
            id: allocate_part_id()?,
            files,
        }));
    }
    let mut message = UserMessage {
        origin: Default::default(),
        transcript_visibility: Default::default(),
        id: message_id,
        parts,
    };
    // 变体上下文作为规范 Part 落盘；恢复和重试复用冻结正文，不能按当前模板重算。
    InternalBoundaryCoordinator::append(
        &mut message,
        InternalBoundaryRequest {
            source: InternalBoundarySource::AgentVariant,
            retention_key: Some("agent_variant".to_owned()),
            text: crate::agent_variant::injection_text(variant).to_owned(),
        },
    )?;
    Ok(message)
}

pub(crate) fn allocate_message_id() -> RuntimeResult<MessageId> {
    id::generate("m")
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "message id random source",
        })
        .and_then(|value| {
            MessageId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "message id generator",
            })
        })
}

pub(crate) fn allocate_part_id() -> RuntimeResult<PartId> {
    id::generate("p")
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "part id random source",
        })
        .and_then(|value| {
            PartId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "part id generator",
            })
        })
}

fn is_active_run(state: &SessionState, run_id: &RunId) -> bool {
    state
        .active_run
        .as_ref()
        .is_some_and(|active| &active.run_id == run_id)
}
