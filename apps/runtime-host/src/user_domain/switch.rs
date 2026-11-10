//! 组织切换由用户域监督任务执行，确认和结果只保留在 Host 内存。

use std::collections::BTreeSet;

use assistant_protocol::{HostOrganization, HostOrganizationSwitchResult, SecretValue};
use assistant_runtime::ShutdownWork;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use tokio::time::Instant;

use super::*;
use crate::{
    access::{LoginSession, enterprise::CenterError},
    http::HttpState,
};

const PREVIEW_LIFETIME: Duration = Duration::from_secs(300);

pub(crate) struct OrganizationSwitch {
    source: UserKey,
    login: [u8; 32],
    organization: HostOrganization,
    created: Instant,
    phase: Mutex<Phase>,
}

enum Phase {
    Preview {
        work: ShutdownWork,
        terminals: BTreeSet<String>,
    },
    Stopping,
    Exchanging,
    Ended(Instant, SwitchOutcome),
}

#[derive(Clone)]
pub(crate) enum SwitchOutcome {
    Changed,
    Stopping,
    Exchanging,
    StopFailed,
    ExchangeFailed,
    Reauthenticate,
    Switched(SecretValue, LoginSession),
}

impl OrganizationSwitch {
    fn outcome(&self) -> Option<SwitchOutcome> {
        match &*self.phase.lock().expect("switch state poisoned") {
            Phase::Preview { .. } => None,
            Phase::Stopping => Some(SwitchOutcome::Stopping),
            Phase::Exchanging => Some(SwitchOutcome::Exchanging),
            Phase::Ended(
                _,
                outcome @ (SwitchOutcome::StopFailed | SwitchOutcome::ExchangeFailed),
            ) => Some(outcome.clone()),
            Phase::Ended(at, outcome) if at.elapsed() < PREVIEW_LIFETIME => Some(outcome.clone()),
            Phase::Ended(..) => None,
        }
    }

    fn blocks_source(&self) -> bool {
        matches!(
            self.outcome(),
            Some(
                SwitchOutcome::Stopping
                    | SwitchOutcome::Exchanging
                    | SwitchOutcome::StopFailed
                    | SwitchOutcome::ExchangeFailed
            )
        )
    }

    fn end(&self, outcome: SwitchOutcome) {
        *self.phase.lock().expect("switch state poisoned") = Phase::Ended(Instant::now(), outcome);
    }
}

pub(super) struct SwitchJob {
    state: HttpState,
    permit: AccessPermit,
    key: UserKey,
    operation: Arc<OrganizationSwitch>,
    work: ShutdownWork,
    terminals: BTreeSet<String>,
}

// 监督任务异常退出时保留明确失败结果，状态查询不会继续显示进行中。
struct SwitchCompletion(Arc<OrganizationSwitch>);
impl Drop for SwitchCompletion {
    fn drop(&mut self) {
        match self.0.outcome() {
            Some(SwitchOutcome::Stopping) => self.0.end(SwitchOutcome::StopFailed),
            Some(SwitchOutcome::Exchanging) => self.0.end(SwitchOutcome::Reauthenticate),
            _ => {}
        }
    }
}

impl UserDomains {
    pub(crate) async fn preview_switch(
        &self,
        permit: &AccessPermit,
        organization: HostOrganization,
        terminals: &crate::user_terminal::UserTerminals,
    ) -> Result<HostOrganizationSwitchResult, DomainError> {
        let key = permit.user_key().ok_or(AccessError::Unauthorized)?;
        let login = permit.login_id().ok_or(AccessError::Unauthorized)?;
        let services = self.ensure(permit).await?;
        let _terminal_admission = terminals.source_gate.lock().await;
        let terminals = terminals.active_ids(key).await;
        let terminal_count = terminals.len();
        let work = services
            .runtime
            .preview_shutdown()
            .await
            .map_err(|_| DomainError::Shutdown)?;
        let (tasks, approvals, queued) = work.counts();
        let mut random = [0; 24];
        getrandom::fill(&mut random).map_err(|_| AccessError::Unavailable)?;
        let operation_id = URL_SAFE_NO_PAD.encode(random);
        let mut switches = self
            .switches
            .lock()
            .expect("organization switches poisoned");
        switches.retain(|_, operation| {
            match &*operation.phase.lock().expect("switch state poisoned") {
                Phase::Preview { .. } => {
                    operation.source != *key && operation.created.elapsed() < PREVIEW_LIFETIME
                }
                Phase::Ended(at, outcome) => {
                    matches!(
                        outcome,
                        SwitchOutcome::StopFailed | SwitchOutcome::ExchangeFailed
                    ) || at.elapsed() < PREVIEW_LIFETIME
                }
                _ => true,
            }
        });
        if switches
            .values()
            .any(|operation| operation.source == *key && operation.blocks_source())
        {
            return Err(AccessError::Busy.into());
        }
        if switches.len() >= 64 {
            return Err(AccessError::Busy.into());
        }
        switches.insert(
            operation_id.clone(),
            Arc::new(OrganizationSwitch {
                source: key.clone(),
                login,
                organization: organization.clone(),
                created: Instant::now(),
                phase: Mutex::new(Phase::Preview { work, terminals }),
            }),
        );
        Ok(HostOrganizationSwitchResult::Confirm {
            operation_id,
            organization,
            tasks: tasks + terminal_count,
            approvals,
            queued,
        })
    }

    pub(crate) fn confirm_switch(
        &self,
        state: HttpState,
        permit: AccessPermit,
        id: &str,
    ) -> Result<SwitchOutcome, DomainError> {
        let key = permit.user_key().ok_or(AccessError::Unauthorized)?.clone();
        let login = permit.login_id().ok_or(AccessError::Unauthorized)?;
        let switches = self
            .switches
            .lock()
            .expect("organization switches poisoned");
        let Some(operation) = switches
            .get(id)
            .filter(|operation| operation.source == key && operation.login == login)
            .cloned()
        else {
            return Ok(SwitchOutcome::Changed);
        };
        let mut phase = operation.phase.lock().expect("switch state poisoned");
        match &*phase {
            Phase::Preview { .. } if operation.created.elapsed() < PREVIEW_LIFETIME => {}
            Phase::Preview { .. } => return Ok(SwitchOutcome::Changed),
            _ => {
                drop(phase);
                return Ok(operation.outcome().unwrap_or(SwitchOutcome::Changed));
            }
        }
        if switches.values().any(|other| {
            !Arc::ptr_eq(other, &operation)
                && other.source.center_id == key.center_id
                && other.source.user_id == key.user_id
                && matches!(
                    other.outcome(),
                    Some(SwitchOutcome::Stopping | SwitchOutcome::Exchanging)
                )
        }) {
            return Err(AccessError::Busy.into());
        }
        let Phase::Preview { work, terminals } = std::mem::replace(&mut *phase, Phase::Stopping)
        else {
            unreachable!("preview was checked under its lock");
        };
        drop(phase);
        drop(switches);
        if self
            .open
            .send(DomainRequest::Switch(Box::new(SwitchJob {
                state,
                permit,
                key,
                operation: operation.clone(),
                work,
                terminals,
            })))
            .is_err()
        {
            operation.end(SwitchOutcome::Changed);
            return Err(DomainError::Shutdown);
        }
        Ok(SwitchOutcome::Stopping)
    }

    fn find_switch(&self, id: &str, login: [u8; 32]) -> Option<Arc<OrganizationSwitch>> {
        self.switches.lock().expect("organization switches poisoned").get(id).filter(|operation| {
            operation.login == login
                || matches!(operation.outcome(), Some(SwitchOutcome::Switched(token, session)) if session.valid() && crate::access::digest(token.expose()) == login)
        }).cloned()
    }

    pub(crate) fn switch_status(&self, id: &str, login: [u8; 32]) -> Option<SwitchOutcome> {
        self.find_switch(id, login)
            .and_then(|operation| operation.outcome())
    }

    pub(crate) async fn resume_after_switch(
        &self,
        permit: &AccessPermit,
        id: &str,
    ) -> Result<(), DomainError> {
        permit.check()?;
        let key = permit.user_key().ok_or(AccessError::Unauthorized)?;
        let operation = self
            .find_switch(id, permit.login_id().ok_or(AccessError::Unauthorized)?)
            .ok_or(AccessError::Conflict)?;
        let slot = self.switch_slot(key).ok_or(AccessError::Conflict)?;
        let loaded = slot.loaded.lock().await;
        let mut switches = self
            .switches
            .lock()
            .expect("organization switches poisoned");
        if loaded.is_some()
            || operation.source != *key
            || !switches
                .get(id)
                .is_some_and(|current| Arc::ptr_eq(current, &operation))
            || !matches!(
                &*operation.phase.lock().expect("switch state poisoned"),
                Phase::Ended(_, SwitchOutcome::ExchangeFailed)
            )
        {
            return Err(AccessError::Conflict.into());
        }
        switches.remove(id);
        slot.switching.store(false, Ordering::Release);
        Ok(())
    }

    fn switch_slot(&self, key: &UserKey) -> Option<Slot> {
        self.slots
            .lock()
            .expect("user domains poisoned")
            .get(&Some(key.clone()))
            .cloned()
    }

    pub(super) async fn run_switch(&self, job: SwitchJob) {
        let SwitchJob {
            state,
            permit,
            key,
            operation,
            work,
            terminals,
        } = job;
        let _completion = SwitchCompletion(operation.clone());
        let credentials = &state.access.credentials;
        let (source, organizations) = match credentials.switch_organizations(&permit).await {
            Ok(value) => value,
            Err(
                AccessError::Unauthorized
                | AccessError::Center(
                    CenterError::AuthenticationFailed | CenterError::InvalidCredentials,
                ),
            ) => {
                operation.end(SwitchOutcome::Reauthenticate);
                return;
            }
            Err(_) => {
                operation.end(SwitchOutcome::Changed);
                return;
            }
        };
        if !organizations
            .iter()
            .any(|org| org.id == operation.organization.id)
        {
            operation.end(SwitchOutcome::Changed);
            return;
        }

        let Some(slot) = self.switch_slot(&key) else {
            operation.end(SwitchOutcome::Changed);
            return;
        };
        let loaded = slot.loaded.lock().await;
        let Some(domain) = loaded
            .as_ref()
            .filter(|domain| !domain.access.is_cancelled())
        else {
            operation.end(SwitchOutcome::Changed);
            return;
        };
        if permit.check().is_err() {
            operation.end(SwitchOutcome::Reauthenticate);
            return;
        }
        let terminal_admission = state.terminals.source_gate.lock().await;
        if !state.terminals.active_ids(&key).await.is_subset(&terminals) {
            operation.end(SwitchOutcome::Changed);
            return;
        }
        match domain
            .services
            .runtime
            .begin_confirmed_shutdown(&work)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                operation.end(SwitchOutcome::Changed);
                return;
            }
            Err(_) => return,
        }
        slot.switching.store(true, Ordering::Release);
        domain.access.cancel();
        drop(terminal_admission);
        let mut closed = domain.closed.clone();
        drop(loaded);

        let domains_closed = closed
            .wait_for(Option::is_some)
            .await
            .is_ok_and(|result| *result == Some(true));
        if !domains_closed || state.terminals.finish_logout(Some(&key)).await.is_err() {
            operation.end(SwitchOutcome::StopFailed);
            return;
        }
        *operation.phase.lock().expect("switch state poisoned") = Phase::Exchanging;
        let result = credentials
            .exchange_organization(
                &state.access,
                &permit,
                source.clone(),
                &operation.organization.id,
                &self.stopping,
            )
            .await;
        match result {
            Ok((token, session)) => {
                slot.switching.store(false, Ordering::Release);
                operation.end(SwitchOutcome::Switched(token, session));
            }
            Err(AccessError::Center(
                CenterError::InvalidRequest | CenterError::StateChanged | CenterError::Busy,
            )) => {
                operation.end(SwitchOutcome::ExchangeFailed);
            }
            Err(_) => {
                credentials.invalidate_switch_login(&key, &source);
                slot.switching.store(false, Ordering::Release);
                operation.end(SwitchOutcome::Reauthenticate);
            }
        }
    }
}
