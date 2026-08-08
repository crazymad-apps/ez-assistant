//! Runtime Run 领域状态、标识分配与子模块入口。

mod recorder;
mod settlement;
mod supervisor;

pub(crate) use recorder::RuntimeRecorder;
pub(crate) use settlement::settle_run;
pub(crate) use supervisor::supervise_run;

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart,
    UserMessage, UserPart,
};
use assistant_protocol::{
    InputId, RunId, RunSnapshot, RunStatus, RuntimeErrorInfo, RuntimeEvent, SessionId,
    ToolActivitySnapshot,
};
use tokio_util::sync::CancellationToken;

use crate::{RuntimeError, RuntimeResult, StoredRun, id, session::SessionState};

use self::settlement::RunSettlement;

/// Session 内保存的 Runtime Run 权威记录。
pub(crate) struct RunRecord {
    run_id: RunId,
    session_id: SessionId,
    input_id: InputId,
    attempt: u32,
    status: RunStatus,
    cancel_requested: bool,
    reasoning: String,
    text: String,
    tools: Vec<ToolActivitySnapshot>,
    error: Option<RuntimeErrorInfo>,
    message_ids: Vec<MessageId>,
}

impl RunRecord {
    pub(crate) fn accepted(
        run_id: RunId,
        session_id: SessionId,
        input_id: InputId,
        attempt: u32,
        message_ids: Vec<MessageId>,
    ) -> Self {
        Self {
            run_id,
            session_id,
            input_id,
            attempt,
            status: RunStatus::Accepted,
            cancel_requested: false,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
            message_ids,
        }
    }

    pub(crate) fn recovered(run: StoredRun) -> Self {
        Self {
            run_id: run.run_id,
            session_id: run.session_id,
            input_id: run.input_id,
            attempt: run.attempt,
            status: run.status,
            cancel_requested: run.cancel_requested,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: run.error,
            message_ids: run.message_ids,
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

    pub(crate) fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            input_id: self.input_id.clone(),
            attempt: self.attempt,
            status: self.status,
            cancel_requested: self.cancel_requested,
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

    pub(crate) fn mark_cancelling(&mut self) -> bool {
        if self.status.is_terminal() || self.status == RunStatus::Cancelling {
            return false;
        }
        self.status = RunStatus::Cancelling;
        self.cancel_requested = true;
        true
    }

    pub(crate) fn input_id(&self) -> &InputId {
        &self.input_id
    }
    pub(crate) fn attempt(&self) -> u32 {
        self.attempt
    }
    pub(crate) fn status(&self) -> RunStatus {
        self.status
    }
    pub(crate) fn fail_before_start(&mut self, error: RuntimeErrorInfo) {
        self.status = RunStatus::Failed;
        self.error = Some(error);
    }

    fn settle(&mut self, settlement: RunSettlement) {
        self.status = settlement.status;
        if let Some(reasoning) = settlement.reasoning {
            self.reasoning = reasoning;
        }
        if let Some(text) = settlement.text {
            self.text = text;
        }
        self.error = settlement.error;
    }
}

/// 当前活动 Run 的执行控制句柄；Session 终态后立即清除。
pub(crate) struct ActiveRun {
    pub(crate) run_id: RunId,
    pub(crate) cancellation: CancellationToken,
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

/// 将已校验的提交文本封装为新的规范 UserMessage。
pub(crate) fn create_user_message(text: String) -> RuntimeResult<UserMessage> {
    let message_id = id::generate("m")
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "message id random source",
        })
        .and_then(|value| {
            MessageId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "message id generator",
            })
        })?;
    let part_id = id::generate("p")
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "part id random source",
        })
        .and_then(|value| {
            PartId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "part id generator",
            })
        })?;
    Ok(UserMessage {
        id: message_id,
        parts: vec![UserPart::Text(TextPart { id: part_id, text })],
    })
}

fn is_active_run(state: &SessionState, run_id: &RunId) -> bool {
    state
        .active_run
        .as_ref()
        .is_some_and(|active| &active.run_id == run_id)
}
