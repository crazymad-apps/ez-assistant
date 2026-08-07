//! 单个 Session 的冻结模型 key、System Prompt 与短临界区状态。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use agent_model::SystemPromptSnapshot;
use agent_types::ConversationSnapshot;
use assistant_protocol::{ModelKey, RunId, RunSnapshot, SessionId, SessionSummary};

use crate::{
    RuntimeError, RuntimeResult, id,
    journal::InMemoryJournal,
    run::{ActiveRun, RunRecord},
};

pub(crate) struct SessionController {
    id: SessionId,
    title: String,
    model_key: ModelKey,
    system_prompt: SystemPromptSnapshot,
    state: Mutex<SessionState>,
}

pub(crate) struct SessionState {
    pub(crate) journal: InMemoryJournal,
    pub(crate) runs: BTreeMap<RunId, RunRecord>,
    pub(crate) active_run: Option<ActiveRun>,
    pub(crate) is_faulted: bool,
}

impl SessionState {
    /// 在写入 UserMessage 和 RunRecord 前统一校验 Session 可变状态。
    pub(crate) fn ensure_can_start(&self, session_id: &SessionId) -> RuntimeResult<()> {
        if self.is_faulted {
            return Err(RuntimeError::SessionFaulted {
                session_id: session_id.clone(),
            });
        }
        if self.active_run.is_some() || self.journal.has_pending() {
            return Err(RuntimeError::SessionBusy {
                session_id: session_id.clone(),
            });
        }
        Ok(())
    }
}

/// 在当前 Session Registry 中分配一个未占用的短标识。
pub(crate) fn allocate_session_id(
    sessions: &BTreeMap<SessionId, Arc<SessionController>>,
) -> RuntimeResult<SessionId> {
    for _ in 0..id::GENERATION_ATTEMPTS {
        let value = id::generate("s").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "session id random source",
        })?;
        let id = SessionId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "session id generator",
        })?;
        if !sessions.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "session id collision",
    })
}

impl SessionController {
    pub(crate) fn new(
        id: SessionId,
        title: String,
        model_key: ModelKey,
        system_prompt: SystemPromptSnapshot,
    ) -> Self {
        Self {
            id,
            title,
            model_key,
            system_prompt,
            state: Mutex::new(SessionState {
                journal: InMemoryJournal::new(),
                runs: BTreeMap::new(),
                active_run: None,
                is_faulted: false,
            }),
        }
    }

    pub(crate) fn summary(&self) -> RuntimeResult<SessionSummary> {
        let state = self.lock_state()?;
        let message_count = u64::try_from(state.journal.message_count()).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "conversation message count",
            }
        })?;
        Ok(SessionSummary {
            session_id: self.id.clone(),
            title: self.title.clone(),
            model_key: self.model_key.clone(),
            active_run_id: state
                .active_run
                .as_ref()
                .map(|active| active.run_id.clone()),
            message_count,
        })
    }

    pub(crate) fn conversation_snapshot(&self) -> RuntimeResult<ConversationSnapshot> {
        Ok(self.lock_state()?.journal.snapshot())
    }

    pub(crate) fn run_snapshot(&self, run_id: &RunId) -> RuntimeResult<RunSnapshot> {
        self.lock_state()?
            .runs
            .get(run_id)
            .map(RunRecord::snapshot)
            .ok_or_else(|| RuntimeError::RunNotFound {
                session_id: self.id.clone(),
                run_id: run_id.clone(),
            })
    }

    pub(crate) fn id(&self) -> &SessionId {
        &self.id
    }

    pub(crate) fn model_key(&self) -> &ModelKey {
        &self.model_key
    }

    pub(crate) fn system_prompt(&self) -> &SystemPromptSnapshot {
        &self.system_prompt
    }

    pub(crate) fn lock_state(&self) -> RuntimeResult<MutexGuard<'_, SessionState>> {
        self.state
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session state",
            })
    }
}
