//! 模型获取与发送共享当前登录身份；晚到结果只能提交到捕获的用户域和登录对象。

use assistant_runtime::{ExternalModelPublication, RuntimeResult};

use super::*;

impl CenterLogin {
    pub(crate) async fn model_configuration(
        &self,
        key: &UserKey,
    ) -> Result<ModelConfiguration, CenterError> {
        self.client
            .model_configuration(&self.token, &key.center_id)
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
    /// 每次发送时重新获取身份；域实例与当前访问状态同时核验，不复用任务启动时的 key。
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
        if !Arc::ptr_eq(domain, &user.activity) || user.connections.is_cancelled() {
            return Err(CenterError::InvalidCredentials);
        }
        Ok(user.login.clone())
    }

    /// 调用方已获取 Runtime 提交门；此处短持身份锁同步提交，释放后由调用方 finish 通知。
    /// 不联网、不等待；返回 false 表示旧登录/域结果已丢弃，需要保留最新加载意图。
    pub(crate) fn publish_model_configuration(
        &self,
        key: &UserKey,
        domain: &Arc<()>,
        login: &Arc<CenterLogin>,
        publication: &mut ExternalModelPublication<'_>,
        result: Result<ModelConfiguration, CenterError>,
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
            Ok(ModelConfiguration::Ready(configuration)) => {
                publication.publish(Some(configuration), "", fetched_at_ms)?
            }
            Ok(ModelConfiguration::Unavailable(reason)) => {
                publication.publish(None, reason, fetched_at_ms)?
            }
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
