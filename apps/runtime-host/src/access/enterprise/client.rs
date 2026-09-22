//! Center 私有协议适配；限制时间、响应体与重定向，不向应用协议暴露后端秘密。

mod models;
pub(crate) use models::ModelConfiguration;

use super::CenterError;
use assistant_protocol::{HostPasswordRequest, HostUserIdentity, SecretValue};
use futures_util::StreamExt as _;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct CenterClient {
    client: Client,
    pub(crate) url: String,
}

#[derive(Clone, Deserialize)]
pub(super) struct Identity {
    pub(super) center_id: String,
    pub(super) user: User,
}

#[derive(Clone, Deserialize)]
pub(super) struct User {
    id: i32,
    username: String,
    display_name: String,
    role: String,
    is_super_admin: bool,
    enabled: bool,
}

#[derive(Deserialize)]
pub(super) struct Login {
    #[serde(flatten)]
    pub(super) identity: Identity,
    pub(super) token: SecretValue,
    pub(super) llm_key: SecretValue,
}

impl Identity {
    pub(super) fn projection(&self) -> Result<HostUserIdentity, CenterError> {
        if !valid_center_id(&self.center_id)
            || self.user.id <= 0
            || self.user.username.is_empty()
            || self.user.username.len() > 128
            || self.user.display_name.len() > 1024
            || !matches!(self.user.role.as_str(), "user" | "admin")
        {
            return Err(CenterError::Unavailable);
        }
        // 权限标记只由中心解释；Host 不据此授予本机管理权限。
        let _ = self.user.is_super_admin;
        if !self.user.enabled {
            return Err(CenterError::InvalidCredentials);
        }
        Ok(HostUserIdentity {
            center_id: self.center_id.clone(),
            user_id: self.user.id,
            username: self.user.username.clone(),
            display_name: self.user.display_name.clone(),
        })
    }
}

pub(super) fn valid_center_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn valid_secret(value: &SecretValue, prefix: &str) -> bool {
    value.expose().strip_prefix(prefix).is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl CenterClient {
    pub(crate) fn new(url: String) -> Result<Self, CenterError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| CenterError::Unavailable)?;
        Ok(Self { client, url })
    }
    fn endpoint(&self, path: &str) -> String {
        format!("{}/api/{path}", self.url.trim_end_matches('/'))
    }

    pub(super) async fn login(
        &self,
        username: String,
        password: SecretValue,
    ) -> Result<Login, CenterError> {
        #[derive(Deserialize)]
        struct Info {
            protocol_version: u32,
            min_protocol_version: u32,
        }
        let info: Info = decode(self.client.get(self.endpoint("info"))).await?;
        if info.min_protocol_version == 0
            || info.min_protocol_version > info.protocol_version
            || !(info.min_protocol_version..=info.protocol_version).contains(&1)
        {
            return Err(CenterError::ProtocolIncompatible);
        }
        let login: Login = decode(
            self.client
                .post(self.endpoint("auth/login"))
                .timeout(Duration::from_secs(15))
                .json(&serde_json::json!({"username":username,"password":password})),
        )
        .await?;
        let validation = login.identity.projection().and_then(|_| {
            if valid_secret(&login.token, "ct_") && valid_secret(&login.llm_key, "cl_") {
                Ok(())
            } else {
                Err(CenterError::Unavailable)
            }
        });
        if let Err(error) = validation {
            if valid_secret(&login.token, "ct_") {
                let _ = self.logout(&login.token).await;
            }
            return Err(error);
        }
        Ok(login)
    }
    pub(super) async fn me(&self, token: &SecretValue) -> Result<Identity, CenterError> {
        decode(
            self.client
                .get(self.endpoint("auth/me"))
                .bearer_auth(token.expose()),
        )
        .await
        .map_err(|error| match error {
            CenterError::AuthenticationFailed => CenterError::InvalidCredentials,
            other => other,
        })
    }
    pub(super) async fn password(
        &self,
        token: &SecretValue,
        request: &HostPasswordRequest,
    ) -> Result<(), CenterError> {
        empty(
            self.client
                .post(self.endpoint("auth/password"))
                .bearer_auth(token.expose())
                .timeout(Duration::from_secs(15))
                .json(request),
        )
        .await
    }
    pub(super) async fn logout(&self, token: &SecretValue) -> Result<(), CenterError> {
        empty(
            self.client
                .post(self.endpoint("auth/logout"))
                .bearer_auth(token.expose())
                .json(&serde_json::json!({})),
        )
        .await
    }
}

async fn response(request: RequestBuilder) -> Result<(StatusCode, Vec<u8>), CenterError> {
    let response = request.send().await.map_err(|_| CenterError::Unavailable)?;
    if response.content_length().is_some_and(|n| n > 65536) {
        return Err(CenterError::Unavailable);
    }
    let status = response.status();
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CenterError::Unavailable)?;
        if body.len() + chunk.len() > 65536 {
            return Err(CenterError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let code = body
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|v| v.as_str());
        return Err(match (status, code) {
            (StatusCode::UNAUTHORIZED, Some("AUTH_FAILED")) => CenterError::AuthenticationFailed,
            (StatusCode::UNAUTHORIZED, _) => CenterError::InvalidCredentials,
            (StatusCode::BAD_REQUEST, _) => CenterError::InvalidRequest,
            (StatusCode::CONFLICT, Some("AUTH_STATE_CHANGED")) => CenterError::StateChanged,
            (StatusCode::TOO_MANY_REQUESTS, _) => CenterError::Busy,
            _ => CenterError::Unavailable,
        });
    }
    Ok((status, body))
}
async fn decode<T: serde::de::DeserializeOwned>(request: RequestBuilder) -> Result<T, CenterError> {
    let (status, body) = response(request).await?;
    if status != StatusCode::OK {
        return Err(CenterError::Unavailable);
    }
    serde_json::from_slice(&body).map_err(|_| CenterError::Unavailable)
}
async fn empty(request: RequestBuilder) -> Result<(), CenterError> {
    let (status, body) = response(request).await?;
    if status != StatusCode::NO_CONTENT || !body.is_empty() {
        return Err(CenterError::Unavailable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{StatusCode, header::LOCATION},
        routing::get,
    };
    #[tokio::test]
    async fn redirects_oversized_bodies_and_unsupported_protocol_never_reach_login() {
        for kind in [0, 1, 2] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let router = match kind {
                0 => Router::new().route(
                    "/api/info",
                    get(|| async { (StatusCode::FOUND, [(LOCATION, "/unexpected")]) }),
                ),
                1 => Router::new().route("/api/info", get(|| async { "x".repeat(65537) })),
                _ => Router::new().route(
                    "/api/info",
                    get(|| async {
                        Json(serde_json::json!({"protocol_version":2,"min_protocol_version":2}))
                    }),
                ),
            };
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let result = CenterClient::new(url)
                .unwrap()
                .login("test".into(), SecretValue::new("password1".into()))
                .await;
            assert!(matches!(
                result,
                Err(CenterError::Unavailable | CenterError::ProtocolIncompatible)
            ));
            task.abort();
        }
    }
    #[test]
    fn identity_and_credential_purposes_are_validated() {
        for id in ["127.0.0.1", "", "01234567-89AB-4cde-8f01-23456789abcd"] {
            assert!(!valid_center_id(id));
        }
        assert!(valid_center_id("01234567-89ab-4cde-8f01-23456789abcd"));
        assert!(valid_secret(
            &SecretValue::new(format!("ct_{}", "a".repeat(64))),
            "ct_"
        ));
        assert!(!valid_secret(
            &SecretValue::new(format!("cl_{}", "a".repeat(64))),
            "ct_"
        ));
        assert!(!valid_secret(
            &SecretValue::new(format!("ct_{}", "A".repeat(64))),
            "ct_"
        ));
    }
}
