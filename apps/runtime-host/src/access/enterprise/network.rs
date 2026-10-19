//! 目标登录复用唯一 UserState；同一用户初始化串行，显式登录和退出优先。
use super::{CenterClient, CenterError, CenterLogin, Credentials, UserKey, UserState, end_user};
use crate::access::{AccessError, AccessPermit, credentials::issue};
use assistant_protocol::{ClientCompatibility, SecretValue};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

pub(in crate::access) struct Initialization {
    gate: Arc<AsyncMutex<()>>,
    pub(super) cancelled: CancellationToken,
}

pub(crate) struct NetworkUser {
    permit: Option<AccessPermit>,
    credentials: Arc<Credentials>,
    key: UserKey,
    init: Arc<Initialization>,
    candidate: Option<Arc<CenterLogin>>,
    guard: Option<OwnedMutexGuard<()>>,
}
impl NetworkUser {
    pub(crate) fn permit(&self) -> &AccessPermit {
        self.permit.as_ref().expect("initialized user has permit")
    }
    pub(crate) fn commit(mut self) -> Result<AccessPermit, AccessError> {
        self.permit().check()?;
        if self.init.cancelled.is_cancelled() {
            return Err(AccessError::Unauthorized);
        }
        self.candidate = None;
        Ok(self.permit.take().expect("initialized user has permit"))
    }
}
impl Drop for NetworkUser {
    fn drop(&mut self) {
        if let Some(permit) = &self.permit {
            permit.logout();
        }
        let mut state = self
            .credentials
            .state
            .lock()
            .expect("credential state poisoned");
        if self.candidate.as_ref().is_some_and(|candidate| {
            state
                .users
                .get(&self.key)
                .is_some_and(|user| Arc::ptr_eq(candidate, &user.login))
        }) {
            end_user(&mut state, &self.key);
        }
        if state
            .network_initializations
            .get(&self.key)
            .is_some_and(|init| Arc::ptr_eq(init, &self.init))
            && Arc::strong_count(&self.init) == 2
        {
            state.network_initializations.remove(&self.key);
        }
    }
}
impl Credentials {
    pub(crate) fn network_caller(&self, center_id: &str) -> Option<SecretValue> {
        let state = self.state.lock().ok()?;
        state
            .users
            .iter()
            .find(|(key, user)| key.center_id == center_id && !user.connections.is_cancelled())
            .map(|(_, user)| user.login.token.clone())
    }
    /// 中心已经准入该访问后调用。门闩覆盖安装与 ready 回执，网络等待不持 Credentials 锁。
    /// 现有登录只核验不替换；新登录在 commit 前取消时按对象身份回滚，不能清理后来的本机登录。
    /// 签发结果未知不自动重试，未采用的中心凭据由访问 owner 的 close/断线收尾撤销。
    pub(crate) async fn network_user<F, Fut>(
        self: &Arc<Self>,
        client: &CenterClient,
        key: UserKey,
        cancelled: CancellationToken,
        issue_login: F,
    ) -> Result<NetworkUser, AccessError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value, AccessError>>,
    {
        let init = {
            let mut state = self.state.lock().expect("credential state poisoned");
            state
                .network_initializations
                .entry(key.clone())
                .or_insert_with(|| {
                    Arc::new(Initialization {
                        gate: Arc::new(AsyncMutex::new(())),
                        cancelled: CancellationToken::new(),
                    })
                })
                .clone()
        };
        let mut initialized = NetworkUser {
            permit: None,
            credentials: self.clone(),
            key: key.clone(),
            init,
            candidate: None,
            guard: None,
        };
        initialized.guard = Some(tokio::select! {
            () = cancelled.cancelled() => return Err(AccessError::Unauthorized),
            () = initialized.init.cancelled.cancelled() => return Err(AccessError::Unauthorized),
            guard = initialized.init.gate.clone().lock_owned() => guard,
        });
        let exists = self
            .state
            .lock()
            .expect("credential state poisoned")
            .users
            .contains_key(&key);
        if !exists {
            let value = tokio::select! {
                () = cancelled.cancelled() => return Err(AccessError::Unauthorized),
                () = initialized.init.cancelled.cancelled() => return Err(AccessError::Unauthorized),
                value = issue_login() => value?,
            };
            let login: super::client::Login =
                serde_json::from_value(value).map_err(|_| AccessError::Unavailable)?;
            if !super::client::valid_secret(&login.token, "ct_")
                || !super::client::valid_secret(&login.llm_key, "cl_")
            {
                return Err(AccessError::Unavailable);
            }
            let identity = login.identity.projection()?;
            if UserKey::from(&identity) != key {
                return Err(CenterError::IdentityMismatch.into());
            }
            let current = Arc::new(CenterLogin {
                client: client.clone(),
                token: login.token,
                llm_key: login.llm_key,
                checking: Mutex::new(None),
            });
            let mut state = self.state.lock().expect("credential state poisoned");
            if cancelled.is_cancelled() || initialized.init.cancelled.is_cancelled() {
                return Err(AccessError::Unauthorized);
            }
            state.users.insert(
                key.clone(),
                UserState {
                    login: current.clone(),
                    identity,
                    connections: CancellationToken::new(),
                    activity: Arc::new(()),
                },
            );
            initialized.candidate = Some(current);
        }
        {
            let mut state = self.state.lock().expect("credential state poisoned");
            if cancelled.is_cancelled() || initialized.init.cancelled.is_cancelled() {
                return Err(AccessError::Unauthorized);
            }
            let (_, session) = issue(&mut state, ClientCompatibility::current(), Some(key))?;
            initialized.permit = Some(AccessPermit::new(false, Some(session), cancelled));
        }
        self.verify_enterprise(initialized.permit.as_mut().expect("permit installed"))
            .await?;
        Ok(initialized)
    }
}
