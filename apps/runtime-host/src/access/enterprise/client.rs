//! Center 私有协议适配；限制时间、响应体与重定向，不向应用协议暴露后端秘密。

mod agents;
mod feedback;
mod models;
pub(crate) mod plugins;
mod registration;
pub(crate) use registration::RegistrationReply;

use super::CenterError;
use assistant_protocol::{HostOrganization, HostPasswordRequest, HostUserIdentity, SecretValue};
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
    pub(super) legacy_organization_id: String,
    pub(super) scope: Scope,
    pub(super) expires_at: String,
    pub(super) user: User,
}

#[derive(Clone, Deserialize)]
pub(super) struct Scope {
    pub(super) kind: String,
    pub(super) organization_id: Option<String>,
    pub(super) name: Option<String>,
    enabled: Option<bool>,
    member_role: Option<String>,
}

impl Scope {
    fn organization(&self) -> Option<HostOrganization> {
        let id = self.organization_id.as_ref()?;
        let name = self.name.as_ref()?;
        (self.kind == "organization"
            && self.enabled == Some(true)
            && matches!(self.member_role.as_deref(), Some("member" | "admin"))
            && valid_center_id(id)
            && !name.trim().is_empty()
            && name.chars().count() <= 64)
            .then(|| HostOrganization {
                id: id.clone(),
                name: name.clone(),
            })
    }
}

#[derive(Clone, Deserialize)]
pub(super) struct User {
    pub(super) id: i32,
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
    pub(super) llm_key: Option<SecretValue>,
}

impl Identity {
    fn validate(&self) -> Result<(), CenterError> {
        if !valid_center_id(&self.center_id)
            || !valid_center_id(&self.legacy_organization_id)
            || self.user.id <= 0
            || self.user.username.is_empty()
            || self.user.username.len() > 128
            || self.user.display_name.len() > 1024
            || !matches!(self.user.role.as_str(), "user" | "system_admin")
        {
            return Err(CenterError::Unavailable);
        }
        // 权限标记只由中心解释；Host 不据此授予本机管理权限。
        let _ = self.user.is_super_admin;
        if !self.user.enabled {
            return Err(CenterError::InvalidCredentials);
        }
        self.expiration()?;
        Ok(())
    }

    pub(super) fn expiration(&self) -> Result<u64, CenterError> {
        let expires = time::OffsetDateTime::parse(
            &self.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| CenterError::Unavailable)?;
        let now = time::OffsetDateTime::now_utc();
        if expires <= now {
            return Err(CenterError::InvalidCredentials);
        }
        if expires - now > time::Duration::days(7) {
            return Err(CenterError::Unavailable);
        }
        u64::try_from(expires.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| CenterError::Unavailable)
    }

    pub(super) fn projection(&self) -> Result<HostUserIdentity, CenterError> {
        self.validate()?;
        let org = self
            .scope
            .organization()
            .ok_or(CenterError::InvalidCredentials)?;
        Ok(HostUserIdentity {
            center_id: self.center_id.clone(),
            organization_id: org.id,
            organization_name: org.name,
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
pub(super) fn valid_secret(value: &SecretValue, prefix: &str) -> bool {
    value.expose().strip_prefix(prefix).is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) fn valid_jwt(value: &SecretValue) -> bool {
    let value = value.expose();
    value.len() <= 8192
        && value.split('.').count() == 3
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
}

/// 无效声明只是中心故障；只有完整有效且不相交的范围才允许上层要求更新。
fn check_protocol_range(minimum: u32, current: u32) -> Result<(), CenterError> {
    if minimum == 0 || minimum > current || current > 65535 {
        return Err(CenterError::Unavailable);
    }
    if !super::SUPPORTED_PROTOCOLS
        .iter()
        .any(|value| (minimum..=current).contains(value))
    {
        return Err(CenterError::ProtocolIncompatible);
    }
    Ok(())
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
        check_protocol_range(info.min_protocol_version, info.protocol_version)?;
        let login: Login = decode(
            self.client
                .post(self.endpoint("auth/login"))
                .timeout(Duration::from_secs(15))
                .json(&serde_json::json!({"username":username,"password":password})),
        )
        .await?;
        let validation = login.identity.validate().and_then(|()| {
            if valid_jwt(&login.token)
                && login.identity.scope.kind == "unselected"
                && login.identity.scope.organization_id.is_none()
                && login.llm_key.is_none()
            {
                Ok(())
            } else {
                Err(CenterError::Unavailable)
            }
        });
        if let Err(error) = validation {
            if valid_jwt(&login.token) {
                let _ = self.logout(&login.token).await;
            }
            return Err(error);
        }
        Ok(login)
    }

    pub(super) async fn organizations(
        &self,
        token: &SecretValue,
    ) -> Result<Vec<HostOrganization>, CenterError> {
        let scopes: Vec<Scope> = decode(
            self.client
                .get(self.endpoint("auth/scopes?entry=client"))
                .bearer_auth(token.expose()),
        )
        .await?;
        scopes
            .iter()
            .map(|scope| scope.organization().ok_or(CenterError::Unavailable))
            .collect()
    }

    pub(super) async fn select(
        &self,
        token: &SecretValue,
        organization_id: &str,
    ) -> Result<Login, CenterError> {
        decode(self.client.post(self.endpoint("auth/scope"))
            .bearer_auth(token.expose())
            .json(&serde_json::json!({"kind":"organization","entry":"client","organization_id":organization_id}))).await
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

const CENTER_REPLY_BYTES: usize = 65_536;

async fn response(
    request: RequestBuilder,
    limit: usize,
) -> Result<(StatusCode, Vec<u8>), CenterError> {
    let response = request.send().await.map_err(|_| CenterError::Unavailable)?;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(CenterError::Unavailable);
    }
    let status = response.status();
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CenterError::Unavailable)?;
        if body.len() + chunk.len() > limit {
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
            (
                StatusCode::CONFLICT,
                Some("AUTH_STATE_CHANGED" | "DEVICE_CHANGED" | "DEVICE_CONFLICT"),
            ) => CenterError::StateChanged,
            (StatusCode::CONFLICT, Some("DEVICE_CENTER_MISMATCH")) => CenterError::IdentityMismatch,
            (StatusCode::TOO_MANY_REQUESTS, _) => CenterError::Busy,
            _ => CenterError::Unavailable,
        });
    }
    Ok((status, body))
}
async fn decode<T: serde::de::DeserializeOwned>(request: RequestBuilder) -> Result<T, CenterError> {
    decode_with_limit(request, CENTER_REPLY_BYTES).await
}

async fn decode_with_limit<T: serde::de::DeserializeOwned>(
    request: RequestBuilder,
    limit: usize,
) -> Result<T, CenterError> {
    let (status, body) = response(request, limit).await?;
    if status != StatusCode::OK {
        return Err(CenterError::Unavailable);
    }
    serde_json::from_slice(&body).map_err(|_| CenterError::Unavailable)
}
async fn empty(request: RequestBuilder) -> Result<(), CenterError> {
    let (status, body) = response(request, CENTER_REPLY_BYTES).await?;
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
                        Json(serde_json::json!({"protocol_version":1,"min_protocol_version":1}))
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
    fn only_valid_disjoint_protocol_ranges_require_update() {
        for (minimum, current, expected) in [
            (0, 2, Err(CenterError::Unavailable)),
            (3, 2, Err(CenterError::Unavailable)),
            (0, 0, Err(CenterError::Unavailable)),
            (65536, 65536, Err(CenterError::Unavailable)),
            (1, u32::MAX, Err(CenterError::Unavailable)),
            (6, 6, Ok(())),
            (5, 5, Err(CenterError::ProtocolIncompatible)),
            (4, 4, Err(CenterError::ProtocolIncompatible)),
            (1, 1, Err(CenterError::ProtocolIncompatible)),
            (1, 3, Err(CenterError::ProtocolIncompatible)),
            (2, 2, Err(CenterError::ProtocolIncompatible)),
            (3, 3, Err(CenterError::ProtocolIncompatible)),
        ] {
            assert_eq!(check_protocol_range(minimum, current), expected);
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
