//! 当前用户的反馈访问；只保存稳定主体，逐次读取并复核登录代际。
#[cfg(test)]
mod tests;
mod validation;
pub(crate) use validation::content as normalize_content;

use super::{CenterLogin, Credentials, UserKey, end_user};
use assistant_protocol::{
    FeedbackClientEnvironment, FeedbackClientKind, FeedbackContent, FeedbackEnvironment,
    FeedbackFailure as Failure, FeedbackSource, FeedbackSupport, ListSubmittedFeedbackRequest,
    SubmitFeedbackRequest, SubmittedFeedback, SubmittedFeedbackPage,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct FeedbackAccess {
    credentials: Arc<Credentials>,
    key: Option<UserKey>,
    permit: Option<crate::access::AccessPermit>,
}
#[derive(Serialize)]
struct Submission {
    submission_id: String,
    #[serde(flatten)]
    content: FeedbackContent,
    source: FeedbackSource,
    environment: FeedbackEnvironment,
}
impl FeedbackAccess {
    pub(crate) fn new(credentials: Arc<Credentials>, key: Option<UserKey>) -> Self {
        Self {
            credentials,
            key,
            permit: None,
        }
    }
    pub(crate) fn for_permit(
        credentials: Arc<Credentials>,
        permit: crate::access::AccessPermit,
    ) -> Self {
        Self {
            credentials,
            key: permit.user_key().cloned(),
            permit: Some(permit),
        }
    }
    async fn permit_ended(&self) {
        match &self.permit {
            Some(permit) => permit.ended().await,
            None => std::future::pending().await,
        }
    }
    fn login(&self) -> Result<(Arc<CenterLogin>, CancellationToken), Failure> {
        if self
            .permit
            .as_ref()
            .is_some_and(|permit| permit.check().is_err())
        {
            return Err(Failure::IdentityChanged);
        }
        let key = self.key.as_ref().ok_or(Failure::Unsupported)?;
        self.credentials
            .current_center_login(key)
            .map_err(|_| Failure::IdentityChanged)
    }

    async fn request<T>(
        &self,
        login: &Arc<CenterLogin>,
        cancel: &CancellationToken,
        writing: bool,
        operation: impl Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        let changed = if writing {
            Failure::SubmissionUnknown
        } else {
            Failure::IdentityChanged
        };
        let result = tokio::select! { biased; _ = cancel.cancelled() => Err(changed), _ = self.permit_ended() => Err(changed), result = operation => result };
        let key = self.key.as_ref().ok_or(Failure::Unsupported)?;
        let mut state = self
            .credentials
            .state
            .lock()
            .expect("credential state poisoned");
        if self
            .permit
            .as_ref()
            .is_some_and(|permit| permit.check().is_err())
        {
            return Err(changed);
        }
        if !state
            .users
            .get(key)
            .is_some_and(|user| Arc::ptr_eq(&user.login, login) && !user.connections.is_cancelled())
        {
            return Err(changed);
        }
        if matches!(result, Err(Failure::IdentityChanged)) {
            end_user(&mut state, key);
        }
        result
    }
    pub(crate) async fn support(&self) -> Result<FeedbackSupport, Failure> {
        #[derive(Deserialize)]
        struct Info {
            #[serde(default)]
            capabilities: Vec<String>,
        }
        let (login, cancel) = self.login()?;
        let info: Info = self
            .request(
                &login,
                &cancel,
                false,
                login.client.feedback_get(&login.token, "info"),
            )
            .await?;
        Ok(FeedbackSupport {
            available: info.capabilities.iter().any(|value| value == "feedback"),
            host_version: env!("CARGO_PKG_VERSION").into(),
            host_os: std::env::consts::OS.into(),
            host_arch: std::env::consts::ARCH.into(),
        })
    }
    pub(crate) async fn submit(
        &self,
        request: SubmitFeedbackRequest,
        source: FeedbackSource,
    ) -> Result<SubmittedFeedback, Failure> {
        validation::id(&request.submission_id)?;
        let content = normalize_content(request.content)?;
        let mut client = request.client;
        if (source == FeedbackSource::Manual && client.client_kind == FeedbackClientKind::Agent)
            || (source == FeedbackSource::Agent && client.client_kind != FeedbackClientKind::Agent)
        {
            return Err(Failure::InvalidInput);
        }
        validation::optional(&mut client.client_version, 128)?;
        validation::optional(&mut client.client_os, 128)?;
        validation::optional(&mut client.client_arch, 128)?;
        let (login, cancel) = self.login()?;
        if !self.support().await?.available {
            return Err(Failure::Unsupported);
        }
        let (current, _) = self.login()?;
        if !Arc::ptr_eq(&current, &login) {
            return Err(Failure::IdentityChanged);
        }
        let body = Submission {
            submission_id: request.submission_id,
            content,
            source,
            environment: environment(client),
        };
        let detail: SubmittedFeedback = self
            .request(
                &login,
                &cancel,
                true,
                login.client.feedback_post(&login.token, &body),
            )
            .await?;
        if !self.valid_detail(&detail)
            || detail.submission_id != body.submission_id
            || detail.summary.source != source
            || detail.summary.title != body.content.title
            || detail.target != body.content.target
            || detail.description != body.content.description
            || detail.reproduction != body.content.reproduction
            || detail.expected_result != body.content.expected_result
        {
            return Err(Failure::SubmissionUnknown);
        }
        Ok(detail)
    }
    pub(crate) async fn list(
        &self,
        request: ListSubmittedFeedbackRequest,
    ) -> Result<SubmittedFeedbackPage, Failure> {
        if request.offset > 1_000_000 {
            return Err(Failure::InvalidInput);
        }
        let mut path = format!("feedback?limit=20&offset={}", request.offset);
        if let Some(id) = request.submission_id {
            validation::id(&id)?;
            path.push_str(&format!("&submission_id={id}"));
        }
        for (name, value) in [
            ("status", serde_json::to_value(request.status)),
            ("target_kind", serde_json::to_value(request.target_kind)),
        ] {
            if let Ok(serde_json::Value::String(value)) = value {
                path.push_str(&format!("&{name}={value}"));
            }
        }
        let (login, cancel) = self.login()?;
        let page: SubmittedFeedbackPage = self
            .request(
                &login,
                &cancel,
                false,
                login.client.feedback_get(&login.token, &path),
            )
            .await?;
        if page.limit != 20
            || page.offset != request.offset
            || page.items.len() > 20
            || page.items.iter().any(|item| {
                self.key
                    .as_ref()
                    .is_none_or(|key| item.owner_user_id != key.user_id)
                    || validation::id(&item.id).is_err()
                    || item.title.is_empty()
            })
        {
            return Err(Failure::Unavailable);
        }
        Ok(page)
    }
    pub(crate) async fn detail(&self, id: &str) -> Result<SubmittedFeedback, Failure> {
        validation::id(id)?;
        let (login, cancel) = self.login()?;
        let detail: SubmittedFeedback = self
            .request(
                &login,
                &cancel,
                false,
                login
                    .client
                    .feedback_get(&login.token, &format!("feedback/{id}")),
            )
            .await?;
        if !self.valid_detail(&detail) || detail.summary.id != id {
            return Err(Failure::Unavailable);
        }
        Ok(detail)
    }
    fn valid_detail(&self, detail: &SubmittedFeedback) -> bool {
        self.key
            .as_ref()
            .is_some_and(|key| detail.summary.owner_user_id == key.user_id)
            && validation::id(&detail.summary.id).is_ok()
            && validation::id(&detail.submission_id).is_ok()
            && detail.summary.target_kind == detail.target.kind
            && detail.summary.target_name == detail.target.name
    }
}
fn environment(client: FeedbackClientEnvironment) -> FeedbackEnvironment {
    FeedbackEnvironment {
        host_version: Some(env!("CARGO_PKG_VERSION").into()),
        host_os: Some(std::env::consts::OS.into()),
        host_arch: Some(std::env::consts::ARCH.into()),
        client,
    }
}
