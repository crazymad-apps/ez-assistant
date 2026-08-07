//! Runtime Run 领域状态、标识分配与子模块入口。

mod recorder;
mod supervisor;

pub(crate) use recorder::RuntimeRecorder;
pub(crate) use supervisor::{settle_run, supervise_run};

use agent_types::{MessageId, PartId, TextPart, UserMessage, UserPart};
use assistant_protocol::{
    RunId, RunSnapshot, RunStatus, RuntimeErrorInfo, RuntimeEvent, SessionId, ToolActivitySnapshot,
};
use tokio_util::sync::CancellationToken;

use crate::{RuntimeError, RuntimeResult, id, session::SessionState};

/// Session 内保存的 Runtime Run 权威记录。
pub(crate) struct RunRecord {
    run_id: RunId,
    session_id: SessionId,
    status: RunStatus,
    cancel_requested: bool,
    reasoning: String,
    text: String,
    tools: Vec<ToolActivitySnapshot>,
    error: Option<RuntimeErrorInfo>,
}

impl RunRecord {
    pub(crate) fn accepted(run_id: RunId, session_id: SessionId) -> Self {
        Self {
            run_id,
            session_id,
            status: RunStatus::Accepted,
            cancel_requested: false,
            reasoning: String::new(),
            text: String::new(),
            tools: Vec::new(),
            error: None,
        }
    }

    pub(crate) fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
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

/// 将已校验的 StartRun 文本封装为新的规范 UserMessage。
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

struct RunSettlement {
    status: RunStatus,
    reasoning: Option<String>,
    text: Option<String>,
    error: Option<RuntimeErrorInfo>,
}

impl RunSettlement {
    fn terminal(status: RunStatus) -> Self {
        Self {
            status,
            reasoning: None,
            text: None,
            error: None,
        }
    }
}
