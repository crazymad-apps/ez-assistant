//! 单个 Session 的 Agent 所有权与短临界区状态。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use agent_sdk::Agent;
use agent_types::ConversationSnapshot;
use assistant_protocol::{RunId, RunSnapshot, SessionId, SessionSummary};

use crate::{
    RuntimeError, RuntimeResult,
    journal::InMemoryJournal,
    run::{ActiveRun, RunRecord},
};

pub(crate) struct SessionController {
    id: SessionId,
    title: String,
    agent: Arc<Agent>,
    state: Mutex<SessionState>,
}

pub(crate) struct SessionState {
    pub(crate) journal: InMemoryJournal,
    pub(crate) runs: BTreeMap<RunId, RunRecord>,
    pub(crate) active_run: Option<ActiveRun>,
    pub(crate) is_faulted: bool,
}

impl SessionController {
    pub(crate) fn new(id: SessionId, title: String, agent: Agent) -> Self {
        Self {
            id,
            title,
            agent: Arc::new(agent),
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

    pub(crate) fn agent(&self) -> Arc<Agent> {
        self.agent.clone()
    }

    pub(crate) fn lock_state(&self) -> RuntimeResult<MutexGuard<'_, SessionState>> {
        self.state
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session state",
            })
    }
}
