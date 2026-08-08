//! Store 启动投影到 Runtime Session registry 的唯一转换。

use std::{collections::BTreeMap, sync::Arc};

use assistant_protocol::{InputId, SessionId};

use crate::{RecoveredRuntime, RuntimeError, RuntimeResult, session::SessionController};

pub(super) fn recover_sessions(
    recovered: RecoveredRuntime,
) -> RuntimeResult<BTreeMap<SessionId, Arc<SessionController>>> {
    let mut input_sessions = BTreeMap::<InputId, SessionId>::new();
    for input in &recovered.inputs {
        if input_sessions
            .insert(input.input_id.clone(), input.session_id.clone())
            .is_some()
        {
            return Err(invalid_recovery());
        }
    }
    let mut referenced_inputs = std::collections::BTreeSet::new();
    for run in &recovered.runs {
        if input_sessions.get(&run.input_id) != Some(&run.session_id) {
            return Err(invalid_recovery());
        }
        referenced_inputs.insert(run.input_id.clone());
    }
    if input_sessions
        .keys()
        .any(|input_id| !referenced_inputs.contains(input_id))
    {
        return Err(invalid_recovery());
    }
    let mut runs_by_session = BTreeMap::<SessionId, Vec<_>>::new();
    for run in recovered.runs {
        runs_by_session
            .entry(run.session_id.clone())
            .or_default()
            .push(run);
    }
    let mut inputs_by_session = BTreeMap::<SessionId, Vec<_>>::new();
    for input in recovered.inputs {
        inputs_by_session
            .entry(input.session_id.clone())
            .or_default()
            .push(input);
    }

    let mut sessions = BTreeMap::new();
    for stored in recovered.sessions {
        let session_id = stored.session_id.clone();
        let controller = Arc::new(SessionController::recovered(
            stored,
            runs_by_session.remove(&session_id).unwrap_or_default(),
            inputs_by_session.remove(&session_id).unwrap_or_default(),
        ));
        if sessions.insert(session_id, controller).is_some() {
            return Err(invalid_recovery());
        }
    }
    if !runs_by_session.is_empty() || !inputs_by_session.is_empty() {
        return Err(invalid_recovery());
    }
    Ok(sessions)
}

fn invalid_recovery() -> RuntimeError {
    RuntimeError::StorageUnavailable {
        operation: "recover runtime",
        source: None,
    }
}
