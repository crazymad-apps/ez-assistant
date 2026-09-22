//! 每用户一份当前中心凭据；与客户端登录共享同一提交锁，不把凭据绑定到任务。

mod client;
mod models;
#[cfg(test)]
mod tests;

use super::{
    AccessError, AccessPermit, Credentials,
    credentials::{LoginSession, issue},
};
use assistant_protocol::{HostPasswordRequest, HostUserIdentity, SecretValue};
pub(crate) use client::{CenterClient, ModelConfiguration};
use futures_util::{
    FutureExt as _,
    future::{BoxFuture, Shared},
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum CenterError {
    #[error("企业中心暂不可达，请稍后重试。")]
    Unavailable,
    #[error("登录已失效，请重新登录。")]
    InvalidCredentials,
    #[error("账号或密码不正确。")]
    AuthenticationFailed,
    #[error("企业中心身份与本机绑定不一致。")]
    IdentityMismatch,
    #[error("企业中心协议版本不兼容。")]
    ProtocolIncompatible,
    #[error("企业中心不支持模型管理与代理，请升级中心。")]
    ModelCapabilityMissing,
    #[error("密码或账号输入不符合要求。")]
    InvalidRequest,
    #[error("账号状态已改变，请重新提交。")]
    StateChanged,
    #[error("操作过于频繁，请稍后重试。")]
    Busy,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UserKey {
    pub(crate) center_id: String,
    pub(crate) user_id: i32,
}
impl From<&HostUserIdentity> for UserKey {
    fn from(identity: &HostUserIdentity) -> Self {
        Self {
            center_id: identity.center_id.clone(),
            user_id: identity.user_id,
        }
    }
}

// 只保存正在进行的核验，不缓存认证成功的时间窗口。
type Check = Shared<BoxFuture<'static, Result<HostUserIdentity, CenterError>>>;

struct CheckLease<'a> {
    owner: &'a Mutex<Option<Arc<Check>>>,
    check: Arc<Check>,
}
impl Drop for CheckLease<'_> {
    fn drop(&mut self) {
        let mut current = self.owner.lock().expect("center check poisoned");
        // 最后一个请求结束或被取消时丢弃 future；超时不能留下一份供下次复用的旧结果。
        if Arc::strong_count(&self.check) == 2
            && current
                .as_ref()
                .is_some_and(|value| Arc::ptr_eq(value, &self.check))
        {
            *current = None;
        }
    }
}
pub(crate) struct CenterLogin {
    client: CenterClient,
    token: SecretValue,
    // 每次实际模型发送从当前登录读取，不冻结到任务。
    llm_key: SecretValue,
    checking: Mutex<Option<Arc<Check>>>,
}
impl CenterLogin {
    async fn verify(self: &Arc<Self>) -> Result<HostUserIdentity, CenterError> {
        let check = {
            let mut checking = self.checking.lock().expect("center check poisoned");
            checking
                .get_or_insert_with(|| {
                    let client = self.client.clone();
                    let token = self.token.clone();
                    Arc::new(
                        async move { client.me(&token).await?.projection() }
                            .boxed()
                            .shared(),
                    )
                })
                .clone()
        };
        let lease = CheckLease {
            owner: &self.checking,
            check,
        };
        lease.check.as_ref().clone().await
    }
}

pub(super) struct UserState {
    login: Arc<CenterLogin>,
    identity: HostUserIdentity,
    connections: CancellationToken,
    activity: Arc<()>,
}

impl Credentials {
    /// 用户域只捕获访问取消和活动观察句柄，绝不捕获逐登录的 Center 凭据。
    pub(crate) fn domain_access(
        &self,
        key: &UserKey,
    ) -> Result<(CancellationToken, Arc<()>), AccessError> {
        let state = self.state.lock().expect("credential state poisoned");
        let user = state
            .users
            .get(key)
            .ok_or(CenterError::InvalidCredentials)?;
        if user.connections.is_cancelled() {
            return Err(CenterError::Unavailable.into());
        }
        Ok((user.connections.clone(), user.activity.clone()))
    }

    /// 只在 Runtime 已确认没有工作后调用；锁内复核新登录，避免关闭其新取得的凭据。
    pub(crate) fn end_unused_user(&self, key: &UserKey) -> bool {
        let mut state = self.state.lock().expect("credential state poisoned");
        if state
            .sessions
            .values()
            .any(|session| session.user_key.as_ref() == Some(key) && session.valid())
        {
            return false;
        }
        end_user(&mut state, key);
        true
    }

    pub(crate) async fn enterprise_login(
        &self,
        client: &CenterClient,
        access: &super::HostAccessHandle,
        username: String,
        password: SecretValue,
        compatibility: assistant_protocol::ClientCompatibility,
        listener: &CancellationToken,
    ) -> Result<(SecretValue, LoginSession), AccessError> {
        let login = client.login(username, password).await?;
        let identity = login.identity.projection()?;
        // Center 写入不重试；只有新取得且未能发布的凭据做一次尽力撤销。
        let result = async {
            access
                .bind_center(client.url.clone(), identity.center_id.clone())
                .await?;
            let mut state = self.state.lock().expect("credential state poisoned");
            if listener.is_cancelled() {
                return Err(AccessError::Unavailable);
            }
            let key = UserKey::from(&identity);
            let issued = issue(&mut state, compatibility, Some(key.clone()))?;
            let current = Arc::new(CenterLogin {
                client: client.clone(),
                token: login.token.clone(),
                llm_key: login.llm_key.clone(),
                checking: Mutex::new(None),
            });
            if let Some(user) = state.users.get_mut(&key) {
                user.login = current;
                user.identity = identity;
            } else {
                state.users.insert(
                    key,
                    UserState {
                        login: current,
                        identity,
                        connections: CancellationToken::new(),
                        activity: Arc::new(()),
                    },
                );
            }
            state.changes.send_replace(());
            Ok(issued)
        }
        .await;
        if result.is_err() {
            let _ = client.logout(&login.token).await;
        }
        result
    }

    /// 只有仍是当前对象的核验结果可以提交。旧对象的失败不影响新登录。
    pub(crate) async fn verify_enterprise(
        &self,
        permit: &mut AccessPermit,
    ) -> Result<HostUserIdentity, AccessError> {
        let key = permit
            .user_key()
            .ok_or(CenterError::InvalidCredentials)?
            .clone();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(CenterError::Unavailable.into());
            }
            permit.check()?;
            let login = {
                let state = self.state.lock().expect("credential state poisoned");
                state
                    .users
                    .get(&key)
                    .ok_or(CenterError::InvalidCredentials)?
                    .login
                    .clone()
            };
            let result = tokio::time::timeout_at(deadline, login.verify())
                .await
                .unwrap_or(Err(CenterError::Unavailable))
                .and_then(|identity| {
                    if UserKey::from(&identity) != key {
                        Err(CenterError::InvalidCredentials)
                    } else {
                        Ok(identity)
                    }
                });
            let mut state = self.state.lock().expect("credential state poisoned");
            let user = state
                .users
                .get_mut(&key)
                .ok_or(CenterError::InvalidCredentials)?;
            if !Arc::ptr_eq(&user.login, &login) {
                continue;
            }
            match result {
                Ok(identity) => {
                    permit.check()?;
                    user.identity = identity.clone();
                    if user.connections.is_cancelled() {
                        user.connections = CancellationToken::new();
                    }
                    permit.enterprise_access =
                        Some((user.connections.clone(), user.activity.clone()));
                    return Ok(identity);
                }
                Err(CenterError::InvalidCredentials) => {
                    end_user(&mut state, &key);
                    return Err(CenterError::InvalidCredentials.into());
                }
                Err(error) => {
                    user.connections.cancel();
                    return Err(error.into());
                }
            }
        }
    }

    pub(crate) fn identity(&self, permit: &AccessPermit) -> Option<HostUserIdentity> {
        let state = self.state.lock().expect("credential state poisoned");
        state
            .users
            .get(permit.user_key()?)
            .map(|user| user.identity.clone())
    }

    pub(crate) async fn enterprise_password(
        &self,
        permit: &AccessPermit,
        request: &HostPasswordRequest,
    ) -> Result<(), AccessError> {
        let login = {
            let state = self.state.lock().expect("credential state poisoned");
            permit.check()?;
            state
                .users
                .get(permit.user_key().ok_or(CenterError::InvalidCredentials)?)
                .ok_or(CenterError::InvalidCredentials)?
                .login
                .clone()
        };
        let result = login.client.password(&login.token, request).await;
        if result == Err(CenterError::InvalidCredentials) {
            let mut state = self.state.lock().expect("credential state poisoned");
            if let Some(key) = permit.user_key()
                && state
                    .users
                    .get(key)
                    .is_some_and(|user| Arc::ptr_eq(&user.login, &login))
            {
                end_user(&mut state, key);
            }
        }
        result.map_err(Into::into)
    }

    pub(crate) async fn enterprise_logout(&self, permit: &AccessPermit) -> bool {
        let login = {
            let mut state = self.state.lock().expect("credential state poisoned");
            let Some(key) = permit.user_key() else {
                return false;
            };
            // 旧会话重复退出不能清除其退出之后创建的新用户状态。
            if permit.check().is_err() {
                return false;
            }
            end_user(&mut state, key).map(|user| user.login)
        };
        match login {
            Some(login) => login.client.logout(&login.token).await.is_ok(),
            None => true,
        }
    }

    /// 唯一 Host 核验循环只关注仍被传输或 Runtime 工作持有的用户。
    pub(crate) async fn recheck_connections(&self, shutdown: CancellationToken) {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { () = shutdown.cancelled() => break, _ = ticks.tick() => {} }
            let checks = {
                let state = self.state.lock().expect("credential state poisoned");
                state
                    .users
                    .iter()
                    .filter(|(_, user)| Arc::strong_count(&user.activity) > 1)
                    .map(|(key, user)| (key.clone(), user.login.clone()))
                    .collect::<Vec<_>>()
            };
            let mut pending = futures_util::stream::FuturesUnordered::new();
            for (key, login) in checks {
                pending.push(async move {
                    let result = login.verify().await;
                    (key, login, result)
                });
            }
            use futures_util::StreamExt as _;
            loop {
                let next = tokio::select! { () = shutdown.cancelled() => return, next = pending.next() => next };
                let Some((key, login, result)) = next else {
                    break;
                };
                let mut state = self.state.lock().expect("credential state poisoned");
                let Some(user) = state.users.get_mut(&key) else {
                    continue;
                };
                if !Arc::ptr_eq(&login, &user.login) {
                    continue;
                }
                match result {
                    Ok(identity) if UserKey::from(&identity) == key => user.identity = identity,
                    Ok(_) | Err(CenterError::InvalidCredentials) => {
                        end_user(&mut state, &key);
                    }
                    Err(_) => user.connections.cancel(),
                }
            }
        }
    }
}
fn end_user(state: &mut super::credentials::CredentialState, key: &UserKey) -> Option<UserState> {
    let user = state.users.remove(key)?;
    user.connections.cancel();
    state.sessions.retain(|_, session| {
        if session.user_key.as_ref() == Some(key) {
            session.cancelled.cancel();
            false
        } else {
            true
        }
    });
    state.changes.send_replace(());
    Some(user)
}
