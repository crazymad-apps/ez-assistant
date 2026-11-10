//! 模型获取与发送共享当前登录身份；晚到结果只能提交到捕获的用户域和登录对象。

use assistant_runtime::{ExternalModelPublication, ExternalModelSet, RuntimeResult};

use super::*;

impl CenterLogin {
    pub(crate) async fn model_configuration(
        &self,
        key: &UserKey,
    ) -> Result<ExternalModelSet, CenterError> {
        self.client
            .model_configuration(&self.token, &key.center_id, &key.organization_id)
            .await
    }

    pub(crate) fn model_authorization(&self) -> String {
        format!("Bearer {}", self.llm_key.expose())
    }

    pub(crate) fn center_url(&self) -> &str {
        &self.client.url
    }
}

impl Credentials {
    /// 每次发送时重新获取身份与用户域生命周期，不复用任务启动时的 key。
    /// 客户端访问连接中断不等于模型认证失败；请求由中心核验，网络错误沿模型有限重试处理。
    pub(crate) fn model_login(
        &self,
        key: &UserKey,
        domain: &Arc<()>,
    ) -> Result<Arc<CenterLogin>, CenterError> {
        let state = self.state.lock().expect("credential state poisoned");
        let user = state
            .users
            .get(key)
            .ok_or(CenterError::InvalidCredentials)?;
        if !Arc::ptr_eq(domain, &user.activity)
            || user.connections.is_cancelled()
            || user.login.expires_at_ms <= crate::access::credentials::current_time_ms()?
        {
            return Err(CenterError::InvalidCredentials);
        }
        Ok(user.login.clone())
    }

    /// 个人直连没有中心代理可代为鉴权，复用 F01 的同登录身份核验。
    /// 仅网络不可用时沿用尚有效的本地域；旧登录的迟到拒绝不会撤销新登录。
    pub(crate) async fn verify_personal_model_login(
        &self,
        key: &UserKey,
        domain: &Arc<()>,
    ) -> Result<(), CenterError> {
        let login = self.model_login(key, domain)?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), login.verify())
            .await
            .unwrap_or(Err(CenterError::Unavailable))
            .and_then(|identity| {
                if UserKey::from(&identity) == *key {
                    Ok(())
                } else {
                    Err(CenterError::InvalidCredentials)
                }
            });
        let current = self.model_login(key, domain)?;
        if !Arc::ptr_eq(&login, &current) {
            return Err(CenterError::StateChanged);
        }
        match result {
            Ok(()) | Err(CenterError::Unavailable) => Ok(()),
            Err(error) => {
                if matches!(
                    error,
                    CenterError::InvalidCredentials | CenterError::IdentityMismatch
                ) {
                    self.reject_model_login(key, domain, &login);
                }
                Err(error)
            }
        }
    }

    /// 调用方已获取 Runtime 提交门；此处短持身份锁同步提交，释放后由调用方 finish 通知。
    /// 不联网、不等待；返回 false 表示旧登录/域结果已丢弃，需要保留最新加载意图。
    pub(crate) fn publish_model_configuration(
        &self,
        key: &UserKey,
        domain: &Arc<()>,
        login: &Arc<CenterLogin>,
        publication: &mut ExternalModelPublication<'_>,
        result: Result<ExternalModelSet, CenterError>,
        fetched_at_ms: i64,
    ) -> RuntimeResult<bool> {
        let mut state = self.state.lock().expect("credential state poisoned");
        if !state.users.get(key).is_some_and(|user| {
            Arc::ptr_eq(domain, &user.activity)
                && Arc::ptr_eq(login, &user.login)
                && !user.connections.is_cancelled()
        }) {
            return Ok(false);
        }
        match result {
            Ok(configuration) => publication.publish(configuration, fetched_at_ms)?,
            Err(error) => {
                publication.refresh_failed(&error.to_string())?;
                if error == CenterError::InvalidCredentials {
                    end_user(&mut state, key);
                }
            }
        }
        Ok(true)
    }

    /// 只让发送时捕获且仍为当前的登录失效，旧凭据的拒绝不能退出新登录。
    pub(crate) fn reject_model_login(
        &self,
        key: &UserKey,
        domain: &Arc<()>,
        login: &Arc<CenterLogin>,
    ) {
        let mut state = self.state.lock().expect("credential state poisoned");
        if state.users.get(key).is_some_and(|user| {
            Arc::ptr_eq(domain, &user.activity) && Arc::ptr_eq(login, &user.login)
        }) {
            end_user(&mut state, key);
        }
    }
}
