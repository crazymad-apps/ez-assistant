//! 同一会话的首次装配入口，供客户端操作与主控工具共享。

use crate::{
    RuntimeError, RuntimeResult, RuntimeStore, StoredAttachment,
    delegation::ChildTaskRegistry,
    permission::{PermissionCoordinator, PermissionFileScope},
    session::SessionController,
};
use assistant_protocol::{AttachmentId, SessionId};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex as RegistryMutex, RwLock, Weak},
};
use tokio::sync::Mutex;

pub(super) struct SessionLoader {
    sessions: Arc<RwLock<BTreeMap<SessionId, Arc<SessionController>>>>,
    attachments: Arc<RwLock<BTreeMap<AttachmentId, StoredAttachment>>>,
    children: Arc<ChildTaskRegistry>,
    store: Arc<dyn RuntimeStore>,
    permissions: Arc<PermissionCoordinator>,
    // 只协调同一会话；主控跨会话访问不能等待一个全局装配锁。
    gates: RegistryMutex<BTreeMap<SessionId, Weak<Mutex<()>>>>,
}

impl SessionLoader {
    pub fn new(
        sessions: Arc<RwLock<BTreeMap<SessionId, Arc<SessionController>>>>,
        attachments: Arc<RwLock<BTreeMap<AttachmentId, StoredAttachment>>>,
        children: Arc<ChildTaskRegistry>,
        store: Arc<dyn RuntimeStore>,
        permissions: Arc<PermissionCoordinator>,
    ) -> Self {
        Self {
            sessions,
            attachments,
            children,
            store,
            permissions,
            gates: RegistryMutex::new(BTreeMap::new()),
        }
    }

    fn loading_gate(&self, id: &SessionId) -> RuntimeResult<Arc<Mutex<()>>> {
        let mut gates = self.gates.lock().map_err(|_| unavailable())?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(id).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(id.clone(), Arc::downgrade(&gate));
        Ok(gate)
    }

    fn release_unused_history(&self) -> RuntimeResult<()> {
        let mut sessions = self.sessions.write().map_err(|_| unavailable())?;
        if sessions.len() < 32 {
            return Ok(());
        }
        let candidates = sessions
            .iter()
            .filter_map(|(id, session)| {
                if Arc::strong_count(session) != 1 {
                    return None;
                }
                let state = session.lock_state().ok()?;
                // 已执行会话的用户暂停等进程内状态仍保留；只释放纯历史查询缓存。
                (!state.execution_prepared).then(|| id.clone())
            })
            .take(sessions.len().saturating_sub(31))
            .collect::<Vec<_>>();
        for id in candidates {
            sessions.remove(&id);
            self.children.remove_session(&id)?;
            self.attachments
                .write()
                .map_err(|_| unavailable())?
                .retain(|_, attachment| attachment.session_id != id);
        }
        Ok(())
    }

    pub fn cached(&self, id: &SessionId) -> RuntimeResult<Option<Arc<SessionController>>> {
        Ok(self
            .sessions
            .read()
            .map_err(|_| unavailable())?
            .get(id)
            .cloned())
    }

    pub async fn prepare(&self, id: &SessionId) -> RuntimeResult<Arc<SessionController>> {
        let _phase = crate::request_diagnostics::request_phase(
            crate::request_diagnostics::RequestPhase::SessionPreparation,
        );
        let gate = self.loading_gate(id)?;
        let _gate = gate.lock().await;
        let cached = self.cached(id)?;
        let mutation = match cached.as_ref() {
            Some(session) => Some(session.mutation().await),
            None => None,
        };
        if let Some(session) = cached.as_ref()
            && session.lock_state()?.execution_prepared
        {
            return Ok(session.clone());
        }
        let mut loaded = self
            .store
            .prepare_session_execution(id)
            .await
            .map_err(|e| RuntimeError::from_store("prepare session execution", e))?;
        let recoveries =
            super::recovery::prepare_interrupted_run_settlements(&loaded.state, super::now_ms()?)?;
        let changed = !recoveries.is_empty();
        for recovery in recoveries {
            match recovery {
                super::recovery::PreparedRunRecovery::Settlement(settlement) => {
                    self.store.settle_run(*settlement).await.map_err(|e| {
                        RuntimeError::from_store("settle interrupted session run", e)
                    })?;
                }
                super::recovery::PreparedRunRecovery::TerminalInput(reconciliation) => {
                    self.store
                        .reconcile_terminal_run_input(reconciliation)
                        .await
                        .map_err(|e| RuntimeError::from_store("reconcile terminal run input", e))?;
                }
            }
        }
        if changed {
            loaded = self
                .store
                .load_session_state(id)
                .await
                .map_err(|e| RuntimeError::from_store("reload prepared session", e))?;
            loaded.execution_window = Some(
                self.store
                    .load_execution_window(id)
                    .await
                    .map_err(|e| RuntimeError::from_store("reload execution window", e))?,
            );
        }
        let execution_window = loaded.execution_window.take();
        let completed_runs = completed_history_runs(&loaded.state);
        let mut recovered = super::recovery::recover_session_registries(loaded)?;
        let prepared =
            recovered
                .sessions
                .remove(id)
                .ok_or_else(|| RuntimeError::SessionNotFound {
                    session_id: id.clone(),
                })?;
        let session = if let Some(session) = cached.as_ref() {
            let prepared = Arc::try_unwrap(prepared).map_err(|_| unavailable())?;
            session.install_prepared_state(prepared)?;
            session.clone()
        } else {
            prepared.lock_state()?.execution_prepared = true;
            prepared
        };
        if let Some(window) = execution_window {
            session.install_execution_window(window)?;
        }
        let requests = self
            .store
            .load_feedback(id)
            .await
            .map_err(|e| RuntimeError::from_store("load feedback", e))?;
        for mut request in requests {
            if request.status == assistant_protocol::FeedbackStatus::Pending
                && (request.mode == assistant_protocol::FeedbackMode::Wait
                    || session
                        .lock_state()?
                        .runs
                        .get(&request.run_id)
                        .is_none_or(|run| run.status() != assistant_protocol::RunStatus::Completed)
                        && !completed_runs.contains(&request.run_id))
            {
                request.status = assistant_protocol::FeedbackStatus::Invalidated;
                self.store
                    .mutate_feedback(crate::FeedbackMutation {
                        request: request.clone(),
                        expected_status: Some(assistant_protocol::FeedbackStatus::Pending),
                    })
                    .await
                    .map_err(|e| RuntimeError::from_store("invalidate interrupted feedback", e))?;
            }
            session
                .lock_state()?
                .feedback
                .insert(request.request_id.clone(), request);
        }
        self.permissions
            .register_scope(PermissionFileScope::Session(id.clone()))
            .await?;
        // 尚未执行的缓存只包含上次进程的诊断控制；准备已完成恢复，不能保留旧中断投影
        // 覆盖文件中的可靠终态。本入口在 execution_prepared 后提前返回，不触及活动 child。
        self.children.remove_session(id)?;
        for child in recovered.child_tasks.into_values() {
            self.children.upsert(child)?;
        }
        self.attachments
            .write()
            .map_err(|_| unavailable())?
            .extend(recovered.attachments);
        if cached.is_none() {
            self.release_unused_history()?;
            self.sessions
                .write()
                .map_err(|_| unavailable())?
                .insert(id.clone(), session.clone());
        }
        session.ensure_healthy()?;
        drop(mutation);
        Ok(session)
    }

    pub async fn load(&self, id: &SessionId) -> RuntimeResult<Arc<SessionController>> {
        let _phase = crate::request_diagnostics::request_phase(
            crate::request_diagnostics::RequestPhase::SessionLoad,
        );
        if let Some(session) = self.cached(id)? {
            return Ok(session);
        }
        let gate = self.loading_gate(id)?;
        let _gate = gate.lock().await;
        if let Some(session) = self.cached(id)? {
            return Ok(session);
        }
        let loaded = self
            .store
            .load_session_state(id)
            .await
            .map_err(|e| RuntimeError::from_store("load session", e))?;
        if loaded.state.sessions.is_empty() {
            return Err(RuntimeError::SessionNotFound {
                session_id: id.clone(),
            });
        }
        let completed_runs = completed_history_runs(&loaded.state);
        let mut recovered = super::recovery::recover_session_registries(loaded)?;
        let session = recovered.sessions.remove(id).ok_or_else(unavailable)?;
        let requests = self
            .store
            .load_feedback(id)
            .await
            .map_err(|e| RuntimeError::from_store("load feedback", e))?;
        for mut request in requests {
            if request.status == assistant_protocol::FeedbackStatus::Pending
                && (request.mode == assistant_protocol::FeedbackMode::Wait
                    || session
                        .lock_state()?
                        .runs
                        .get(&request.run_id)
                        .is_none_or(|run| run.status() != assistant_protocol::RunStatus::Completed)
                        && !completed_runs.contains(&request.run_id))
            {
                request.status = assistant_protocol::FeedbackStatus::Invalidated;
            }
            session
                .lock_state()?
                .feedback
                .insert(request.request_id.clone(), request);
        }
        self.permissions
            .register_scope(PermissionFileScope::Session(id.clone()))
            .await?;
        for child in recovered.child_tasks.into_values() {
            self.children.upsert(child)?;
        }
        self.attachments
            .write()
            .map_err(|_| unavailable())?
            .extend(recovered.attachments);
        self.release_unused_history()?;
        self.sessions
            .write()
            .map_err(|_| unavailable())?
            .insert(id.clone(), session.clone());
        Ok(session)
    }
}

fn unavailable() -> RuntimeError {
    RuntimeError::InternalStateUnavailable {
        component: "session registry",
    }
}

fn completed_history_runs(
    state: &crate::RecoveredRuntime,
) -> std::collections::BTreeSet<assistant_protocol::RunId> {
    state
        .run_history
        .iter()
        .filter(|(_, run)| {
            !run.inherited
                && !run.partial
                && run.status == Some(assistant_protocol::RunStatus::Completed)
        })
        .map(|(_, run)| run.run_id.clone())
        .collect()
}
