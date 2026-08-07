//! Core 两阶段 Recorder 到 Runtime Session/Run Journal 的绑定。

use std::sync::Arc;

use agent_core::{ExchangeReceipt, ExecutionRecorder, RecordError, RecordFuture};
use agent_types::{AssistantMessage, ToolMessage};
use assistant_protocol::RunId;

use crate::session::{SessionController, SessionState};

use super::is_active_run;

/// 把 Core 的两阶段落账调用绑定到唯一 Session/Run。
pub(crate) struct RuntimeRecorder {
    session: Arc<SessionController>,
    run_id: RunId,
}

impl RuntimeRecorder {
    pub(crate) fn new(session: Arc<SessionController>, run_id: RunId) -> Self {
        Self { session, run_id }
    }
}

impl ExecutionRecorder for RuntimeRecorder {
    fn begin_tool_exchange<'a>(
        &'a self,
        assistant: AssistantMessage,
    ) -> RecordFuture<'a, ExchangeReceipt> {
        Box::pin(async move {
            let mut state = self.lock_state()?;
            if !is_active_run(&state, &self.run_id) {
                state.is_faulted = true;
                return Err(record_error("active run does not match recorder"));
            }
            match state.journal.begin_tool_exchange(&self.run_id, assistant) {
                Ok(receipt) => Ok(receipt),
                Err(_) => {
                    state.is_faulted = true;
                    Err(record_error("journal rejected tool exchange begin"))
                }
            }
        })
    }

    fn complete_tool_exchange<'a>(
        &'a self,
        receipt: &'a ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.lock_state()?;
            if !is_active_run(&state, &self.run_id) {
                state.is_faulted = true;
                return Err(record_error("active run does not match recorder"));
            }
            match state
                .journal
                .complete_tool_exchange(&self.run_id, receipt, results)
            {
                Ok(()) => Ok(()),
                Err(_) => {
                    state.is_faulted = true;
                    Err(record_error("journal rejected tool exchange completion"))
                }
            }
        })
    }
}

impl RuntimeRecorder {
    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, SessionState>, RecordError> {
        self.session
            .lock_state()
            .map_err(|_| record_error("session state is unavailable"))
    }
}

fn record_error(message: &'static str) -> RecordError {
    RecordError {
        message: message.to_owned(),
    }
}
