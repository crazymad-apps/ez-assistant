//! 中心插件目录与精确包下载；网络错误不变成静默版本替换。

use std::time::Duration;

use assistant_protocol::{
    PluginArchiveFormat, PluginMarketEntry, PluginMarketSnapshot, PluginPackageOrigin,
    PluginPackageSelection, SecretValue,
};
use reqwest::{Response, StatusCode};
use serde::Deserialize;

use super::CenterClient;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PluginCenterError {
    #[error("center plugin catalog is unavailable")]
    Unavailable,
    #[error("center plugin package changed")]
    Changed,
    #[error("center plugin package is not listed")]
    NotListed,
    #[error("center plugin selection is invalid")]
    Invalid,
}

#[derive(Deserialize)]
struct Resolution {
    package: ResolvedPackage,
}

#[derive(Deserialize)]
struct ResolvedPackage {
    name: String,
    version: String,
    archive_format: PluginArchiveFormat,
    sha256: String,
    size_bytes: u64,
}

impl CenterClient {
    pub(crate) async fn list_plugins(
        &self,
        token: &SecretValue,
    ) -> Result<PluginMarketSnapshot, PluginCenterError> {
        let url = self.plugin_url(&[])?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token.expose())
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| PluginCenterError::Unavailable)?;
        if !response.status().is_success() {
            return Err(PluginCenterError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|size| size > 16 * 1024 * 1024)
        {
            return Err(PluginCenterError::Invalid);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| PluginCenterError::Unavailable)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(PluginCenterError::Invalid);
        }
        let plugins: Vec<PluginMarketEntry> =
            serde_json::from_slice(&bytes).map_err(|_| PluginCenterError::Invalid)?;
        Ok(PluginMarketSnapshot { plugins })
    }
    fn plugin_url(&self, segments: &[&str]) -> Result<reqwest::Url, PluginCenterError> {
        let mut url = reqwest::Url::parse(&self.url).map_err(|_| PluginCenterError::Unavailable)?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| PluginCenterError::Unavailable)?;
            path.pop_if_empty();
            path.extend(["api", "runtime", "plugins"]);
            path.extend(segments.iter().copied());
        }
        Ok(url)
    }

    pub(crate) async fn resolve_plugin(
        &self,
        token: &SecretValue,
        center_id: &str,
        name: &str,
        expected: Option<String>,
    ) -> Result<PluginPackageSelection, PluginCenterError> {
        self.resolve_plugin_with_requirements(token, center_id, name, expected, &[], &[])
            .await
    }

    pub(crate) async fn resolve_plugin_with_requirements(
        &self,
        token: &SecretValue,
        center_id: &str,
        name: &str,
        expected: Option<String>,
        required_skills: &[String],
        required_mcp: &[String],
    ) -> Result<PluginPackageSelection, PluginCenterError> {
        let url = self.plugin_url(&[name, "resolve"])?;
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "x64",
            _ => return Err(PluginCenterError::Invalid),
        };
        let response = self.client.post(url).bearer_auth(token.expose()).timeout(Duration::from_secs(15))
            .json(&serde_json::json!({"os": std::env::consts::OS, "arch": arch, "host_version": env!("CARGO_PKG_VERSION"), "required_skills": required_skills, "required_mcp": required_mcp}))
            .send().await.map_err(|_| PluginCenterError::Unavailable)?;
        if response.status() == StatusCode::GONE {
            return Err(PluginCenterError::NotListed);
        }
        if !response.status().is_success() {
            return Err(PluginCenterError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|size| size > 16 * 1024 * 1024)
        {
            return Err(PluginCenterError::Invalid);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| PluginCenterError::Unavailable)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(PluginCenterError::Invalid);
        }
        let result: Resolution =
            serde_json::from_slice(&bytes).map_err(|_| PluginCenterError::Invalid)?;
        if result.package.name != name
            || result.package.size_bytes == 0
            || result.package.size_bytes > 2 * 1024 * 1024 * 1024
        {
            return Err(PluginCenterError::Invalid);
        }
        Ok(PluginPackageSelection {
            name: result.package.name,
            version: result.package.version,
            sha256: result.package.sha256,
            size_bytes: result.package.size_bytes,
            format: result.package.archive_format,
            origin: PluginPackageOrigin::Center {
                center_id: center_id.to_owned(),
            },
            expected_active_sha256: expected,
        })
    }

    pub(crate) async fn download_plugin(
        &self,
        token: &SecretValue,
        choice: &PluginPackageSelection,
    ) -> Result<Response, PluginCenterError> {
        let url = self.plugin_url(&[&choice.name, "versions", &choice.version, "package"])?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| PluginCenterError::Unavailable)?;
        let response = client
            .get(url)
            .bearer_auth(token.expose())
            .header(reqwest::header::IF_MATCH, format!("\"{}\"", choice.sha256))
            .timeout(Duration::from_secs(30 * 60))
            .send()
            .await
            .map_err(|_| PluginCenterError::Unavailable)?;
        match response.status() {
            StatusCode::PRECONDITION_FAILED => return Err(PluginCenterError::Changed),
            StatusCode::GONE => return Err(PluginCenterError::NotListed),
            status if !status.is_success() => return Err(PluginCenterError::Unavailable),
            _ => {}
        }
        if response.content_length() != Some(choice.size_bytes)
            || response
                .headers()
                .get("x-plugin-sha256")
                .and_then(|value| value.to_str().ok())
                != Some(&choice.sha256)
            || response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                != Some(format!("\"{}\"", choice.sha256).as_str())
        {
            return Err(PluginCenterError::Changed);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode, header},
        response::IntoResponse,
        routing::{get, post},
    };

    #[tokio::test]
    async fn market_resolve_and_conditional_download_use_exact_center_package() {
        let sha = "a".repeat(64);
        let market_sha = sha.clone();
        let resolve_sha = sha.clone();
        let download_sha = sha.clone();
        let router = Router::new()
            .route(
                "/api/runtime/plugins",
                get(move || {
                    let market_sha = market_sha.clone();
                    async move { Json(serde_json::json!([{
                        "name":"document-tools", "display_name":"Document Tools", "description":"",
                        "versions":[{"version":"1.0.0", "sha256":market_sha,
                            "size_bytes":7, "archive_format":"tar_gzip", "skills":["document-review"], "mcp":[]}]
                    }])) }
                }),
            )
            .route(
                "/api/runtime/plugins/document-tools/resolve",
                post(move |Json(request): Json<serde_json::Value>| {
                    let resolve_sha = resolve_sha.clone();
                    async move {
                    assert_eq!(request["os"], std::env::consts::OS);
                    assert_eq!(request["host_version"], env!("CARGO_PKG_VERSION"));
                    assert_eq!(request["required_skills"], serde_json::json!(["document-review"]));
                    assert_eq!(request["required_mcp"], serde_json::json!(["review-server"]));
                    Json(serde_json::json!({"package":{
                        "name":"document-tools", "version":"1.0.0", "sha256":resolve_sha,
                        "size_bytes":7, "archive_format":"tar_gzip"
                    }}))
                    }
                }),
            )
            .route(
                "/api/runtime/plugins/document-tools/versions/1.0.0/package",
                get(move |headers: HeaderMap| {
                    let download_sha = download_sha.clone();
                    async move {
                    if headers.get(header::IF_MATCH).and_then(|value| value.to_str().ok())
                        != Some(format!("\"{download_sha}\"").as_str())
                    {
                        return StatusCode::PRECONDITION_FAILED.into_response();
                    }
                    (
                        [
                            (header::ETAG, format!("\"{download_sha}\"")),
                            (header::CONTENT_LENGTH, "7".into()),
                            (header::HeaderName::from_static("x-plugin-sha256"), download_sha),
                        ],
                        "archive",
                    )
                        .into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = CenterClient::new(format!("http://{address}")).unwrap();
        let token = SecretValue::new("test-token".into());
        let market = client.list_plugins(&token).await.unwrap();
        assert_eq!(market.plugins[0].name, "document-tools");
        let selection = client
            .resolve_plugin_with_requirements(
                &token,
                "center-test",
                "document-tools",
                None,
                &["document-review".into()],
                &["review-server".into()],
            )
            .await
            .unwrap();
        assert_eq!(selection.sha256, sha);
        assert_eq!(
            selection.origin,
            PluginPackageOrigin::Center {
                center_id: "center-test".into()
            }
        );
        let response = client.download_plugin(&token, &selection).await.unwrap();
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"archive");
        let mut stale = selection;
        stale.sha256 = "b".repeat(64);
        assert!(matches!(
            client.download_plugin(&token, &stale).await,
            Err(PluginCenterError::Changed)
        ));
        task.abort();
    }
}
