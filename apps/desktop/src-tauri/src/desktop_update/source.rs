//! 来源仅取自当前 Host 的登录前上下文，网络错误不产生强制更新结论。
use super::{UpdateError, UpdateSource};
use assistant_protocol::{ClientCompatibility, DesktopUpdateContext, RuntimeHostCapabilities};
use serde::de::DeserializeOwned;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

pub(super) fn http() -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| UpdateError::Check)
}

pub(super) async fn json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<T, UpdateError> {
    if !response.status().is_success()
        || response.content_length().is_some_and(|n| n > limit as u64)
    {
        return Err(UpdateError::Check);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UpdateError::Check)? {
        if bytes.len() + chunk.len() > limit {
            return Err(UpdateError::Check);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| UpdateError::Check)
}

pub(super) async fn read(
    binding: String,
    origin: &str,
    capabilities: &RuntimeHostCapabilities,
    local: Option<crate::runtime_bootstrap::RuntimeBootstrap>,
    cancellation: CancellationToken,
) -> Result<Option<UpdateSource>, UpdateError> {
    let response = http()?
        .get(format!("{origin}/desktop-update-context"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| UpdateError::Check)?;
    // 旧 Host 没有该接口，不猜测其 Center 或兼容状态。
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let context: DesktopUpdateContext = json(response, 16 * 1024).await?;
    let Some(center) = context.center_url else {
        return Ok(None);
    };
    let center = Url::parse(&center).map_err(|_| UpdateError::Check)?;
    if !matches!(center.scheme(), "http" | "https")
        || center.host_str().is_none()
        || !center.username().is_empty()
        || center.password().is_some()
        || center.query().is_some()
        || center.fragment().is_some()
        || !matches!(center.path(), "" | "/")
        || context.enterprise_protocols.is_empty()
        || context.enterprise_protocols.len() > 16
        || context
            .enterprise_protocols
            .iter()
            .any(|p| *p == 0 || *p > 65535)
    {
        return Err(UpdateError::Check);
    }
    let host = ClientCompatibility {
        version: capabilities.runtime_version.clone(),
        min_compatible_version: capabilities.min_compatible_version.clone(),
    };
    if !host.is_valid() {
        return Err(UpdateError::Check);
    }
    if cancellation.is_cancelled() {
        return Err(UpdateError::Cancelled);
    }
    let remote_origin = local.is_none().then(|| origin.to_owned());
    Ok(Some(UpdateSource {
        binding,
        center,
        host,
        protocols: context.enterprise_protocols,
        local,
        remote_origin,
        cancellation,
    }))
}

impl UpdateSource {
    pub(super) fn endpoint(&self, suffix: &str) -> Result<Url, UpdateError> {
        self.center
            .join(&format!("/api/desktop-updates/{suffix}"))
            .map_err(|_| UpdateError::Check)
    }

    pub(super) async fn center_protocol(&self) -> Result<(u32, u32), UpdateError> {
        let response = http()?
            .get(
                self.center
                    .join("/api/info")
                    .map_err(|_| UpdateError::Check)?,
            )
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| UpdateError::Check)?;
        let value: serde_json::Value = json(response, 32 * 1024).await?;
        let minimum = value["min_protocol_version"]
            .as_u64()
            .ok_or(UpdateError::Check)?;
        let current = value["protocol_version"]
            .as_u64()
            .ok_or(UpdateError::Check)?;
        if minimum == 0 || minimum > current || current > 65535 {
            return Err(UpdateError::Check);
        }
        Ok((minimum as u32, current as u32))
    }

    pub(super) fn supports(&self, range: (u32, u32)) -> bool {
        self.protocols
            .iter()
            .any(|p| range.0 <= *p && *p <= range.1)
    }
}
