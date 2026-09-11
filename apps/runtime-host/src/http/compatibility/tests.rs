//! 真实 HTTP 路由与内存认证；不构造 Runtime Store、不打开数据库、不创建 PTY。

use super::*;
use crate::{
    access::HostAccessService,
    config_source::LocalConfigSource,
    http::{HttpEndpointState, HttpState},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Fixture {
    address: String,
    state: HttpState,
    server: tokio::task::JoinHandle<()>,
    _home: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let (_, access) = HostAccessService::new(Arc::new(LocalConfigSource::new(
            home.path().join("config.toml"),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        let address = format!("http://{authority}");
        let terminals = crate::user_terminal::UserTerminalService::new();
        let state = HttpState::starting(
            HttpEndpointState::new(
                "native-fixture",
                authority,
                address.clone(),
                home.path().into(),
                "instance-fixture".into(),
            ),
            CancellationToken::new(),
            access,
            terminals.handle.clone(),
        );
        let app = crate::http::router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            address,
            state,
            server,
            _home: home,
        }
    }
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .request(method, format!("{}{path}", self.address))
    }
    async fn ordinary_cookie(&self) -> String {
        let issued: Value = self
            .request(Method::POST, "/auth/login")
            .bearer_auth("native-fixture")
            .header(CLIENT_VERSION_HEADER, "0.25.3")
            .header(MIN_COMPATIBLE_VERSION_HEADER, "0.25.2")
            .json(&json!({"method":"desktop"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let token = issued["token"].as_str().unwrap();
        let response = self
            .request(Method::POST, "/auth/login")
            .header(CLIENT_VERSION_HEADER, "0.25.4")
            .header(MIN_COMPATIBLE_VERSION_HEADER, "0.25.2")
            .header("Origin", &self.address)
            .json(&json!({"method":"token","token":token}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let old = self.state.access.credentials.authenticate(token).unwrap();
        let old = AccessPermit::new(false, Some(old), CancellationToken::new());
        assert_eq!(old.compatibility().unwrap().version, "0.25.3");
        let current = self
            .state
            .access
            .credentials
            .authenticate(cookie.split_once('=').unwrap().1)
            .unwrap();
        let current = AccessPermit::new(false, Some(current), CancellationToken::new());
        assert_eq!(current.compatibility().unwrap().version, "0.25.4");
        cookie
    }
}

#[tokio::test]
async fn native_auth_precedes_version_admission_and_all_business_routes_require_headers() {
    let f = Fixture::new().await;
    for (method, path) in [
        (Method::POST, "/commands"),
        (Method::GET, "/events"),
        (Method::POST, "/sessions/fixture/attachments"),
        (Method::POST, "/host-files/download"),
        (Method::GET, "/auth/session"),
    ] {
        assert_eq!(
            f.request(method.clone(), path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response = f
            .request(method, path)
            .bearer_auth("native-fixture")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], "missing_declaration");
    }
    for path in ["/health", "/capabilities"] {
        let response = f
            .request(Method::GET, path)
            .bearer_auth("native-fixture")
            .header(CLIENT_VERSION_HEADER, "invalid-secret")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert!(body.get("protocol_version").is_none());
    }
    assert!(!f.state.shutdown.is_cancelled());
}

#[tokio::test]
async fn pairs_duplicates_invalid_and_both_version_floors_have_safe_errors() {
    let f = Fixture::new().await;
    for (version, minimum, expected) in [
        ("0.25.1", "0.25.1", "client_too_old"),
        ("0.25.3", "0.25.3", "host_too_old"),
        ("invalid-secret", "0.25.2", "invalid_declaration"),
        ("0.25.2", "0.25.3", "invalid_declaration"),
    ] {
        let response = f
            .request(Method::GET, "/auth/session")
            .bearer_auth("native-fixture")
            .header(CLIENT_VERSION_HEADER, version)
            .header(MIN_COMPATIBLE_VERSION_HEADER, minimum)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], expected);
        assert!(!body.to_string().contains("invalid-secret"));
    }
    for duplicate in [false, true] {
        let request = f
            .request(Method::GET, "/auth/session")
            .bearer_auth("native-fixture")
            .header(CLIENT_VERSION_HEADER, "0.25.2");
        let request = if duplicate {
            request
                .header(CLIENT_VERSION_HEADER, "0.25.3")
                .header(MIN_COMPATIBLE_VERSION_HEADER, "0.25.2")
        } else {
            request
        };
        assert_eq!(request.send().await.unwrap().status(), StatusCode::CONFLICT);
    }
    assert_eq!(
        f.request(Method::GET, "/auth/session")
            .bearer_auth("native-fixture")
            .header(CLIENT_VERSION_HEADER, "0.25.3")
            .header(MIN_COMPATIBLE_VERSION_HEADER, "0.25.2")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn cookie_declaration_is_only_a_media_fallback_and_explicit_headers_cannot_bypass() {
    let f = Fixture::new().await;
    let cookie = f.ordinary_cookie().await;
    for path in [
        "/sessions/s/attachments/a/preview",
        "/sessions/s/attachments/a/download",
        "/sessions/s/attachments/a/thumbnail",
        "/sessions/s/messages/m/resources/r/preview",
        "/sessions/s/messages/m/resources/r/download",
        "/sessions/s/child-tasks/c/messages/m/resources/r/preview",
        "/sessions/s/child-tasks/c/messages/m/resources/r/download",
        "/sessions/s/export.md",
    ] {
        // Starting 返回 503 表示已经通过兼容层，仍不能访问未就绪业务。
        assert_eq!(
            f.request(Method::GET, path)
                .header("Cookie", &cookie)
                .header("Origin", &f.address)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}"
        );
        assert_eq!(
            f.request(Method::GET, path)
                .bearer_auth("native-fixture")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            f.request(Method::GET, path)
                .header("Cookie", &cookie)
                .header("Origin", &f.address)
                .header(CLIENT_VERSION_HEADER, "0.25.1")
                .header(MIN_COMPATIBLE_VERSION_HEADER, "0.25.1")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
    }
    for (method, path) in [
        (Method::POST, "/commands"),
        (Method::GET, "/events"),
        (Method::GET, "/auth/session"),
        (Method::POST, "/host-files/download"),
    ] {
        assert_eq!(
            f.request(method, path)
                .header("Cookie", &cookie)
                .header("Origin", &f.address)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        f.request(Method::POST, "/auth/logout")
            .header("Cookie", &cookie)
            .header("Origin", &f.address)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn cors_allows_version_headers_and_failed_login_does_not_issue_a_session() {
    let f = Fixture::new().await;
    let response = f
        .request(Method::OPTIONS, "/commands")
        .header("Origin", &f.address)
        .header("Access-Control-Request-Method", "POST")
        .header(
            "Access-Control-Request-Headers",
            "authorization,x-ez-client-version,x-ez-min-compatible-version",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains(CLIENT_VERSION_HEADER)
    );
    let response = f
        .request(Method::POST, "/auth/login")
        .bearer_auth("native-fixture")
        .json(&json!({"method":"desktop"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!response.headers().contains_key("set-cookie"));
}
