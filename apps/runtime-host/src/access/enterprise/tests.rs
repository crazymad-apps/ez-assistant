//! 可控 Center 与实际 HTTP 路由验证；用户 Runtime 用例仅打开全新 TempDir 人工库。
mod runtime;

use super::*;
use crate::{
    access::HostAccessService,
    config_source::LocalConfigSource,
    http::{HttpEndpointState, HttpState},
};
use assistant_protocol::{ClientCompatibility, SecretValue};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, sync::Notify};
use tokio_util::sync::CancellationToken;

const CENTER_ID: &str = "01234567-89ab-4cde-8f01-23456789abcd";
#[derive(Default)]
struct Mock {
    serial: AtomicUsize,
    tokens: Mutex<HashMap<String, i32>>,
    seen: Mutex<Vec<String>>,
    unavailable: AtomicBool,
    mismatch: AtomicBool,
    logout_calls: AtomicUsize,
    hold: Mutex<Option<Arc<Held>>>,
    logout_hold: Mutex<Option<Arc<Held>>>,
    model_configuration: Mutex<Value>,
    model_hold: Mutex<Option<Arc<Held>>>,
    model_requests: AtomicUsize,
    model_unavailable: AtomicBool,
    proxy_requests: Mutex<Vec<(String, Value)>>,
    proxy_control: Mutex<Option<(u16, String)>>,
    proxy_hold: Mutex<Option<Arc<Held>>>,
}
#[derive(Default)]
struct Held {
    started: Notify,
    release: Notify,
}
fn identity(id: i32, mismatch: bool) -> Value {
    json!({"center_id": if mismatch { "11234567-89ab-4cde-8f01-23456789abcd" } else { CENTER_ID },
        "user":{"id":id,"username":format!("user{id}"),"display_name":format!("User {id}"),"role":"user","is_super_admin":false,"enabled":true}})
}
fn token(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .into()
}
async fn mock_login(State(mock): State<Arc<Mock>>, Json(request): Json<Value>) -> Response {
    if request["password"] != "password1" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":"AUTH_FAILED"}})),
        )
            .into_response();
    }
    let id = if request["username"] == "bob" { 2 } else { 1 };
    let serial = mock.serial.fetch_add(1, Ordering::SeqCst) + 1;
    let token = format!("ct_{serial:064x}");
    mock.tokens.lock().unwrap().insert(token.clone(), id);
    let mut result = identity(id, mock.mismatch.load(Ordering::SeqCst));
    result["token"] = json!(token);
    result["llm_key"] = json!(format!("cl_{serial:064x}"));
    Json(result).into_response()
}
async fn mock_me(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> Response {
    let token = token(&headers);
    mock.seen.lock().unwrap().push(token.clone());
    let id = mock.tokens.lock().unwrap().get(&token).copied();
    let unavailable = mock.unavailable.load(Ordering::SeqCst);
    let held = mock.hold.lock().unwrap().take();
    if let Some(held) = held {
        held.started.notify_one();
        held.release.notified().await;
    }
    if unavailable {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match id {
        Some(id) => Json(identity(id, mock.mismatch.load(Ordering::SeqCst))).into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":"TOKEN_INVALID"}})),
        )
            .into_response(),
    }
}
async fn mock_logout(State(mock): State<Arc<Mock>>, headers: HeaderMap) -> StatusCode {
    mock.logout_calls.fetch_add(1, Ordering::SeqCst);
    let held = mock.logout_hold.lock().unwrap().take();
    if let Some(held) = held {
        held.started.notify_one();
        held.release.notified().await;
    }
    if mock.unavailable.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    mock.tokens.lock().unwrap().remove(&token(&headers));
    StatusCode::NO_CONTENT
}

struct Fixture {
    home: tempfile::TempDir,
    state: HttpState,
    mock: Arc<Mock>,
    url: String,
    client: reqwest::Client,
    shutdown: CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    domain_task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = &self.domain_task {
            task.abort();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_domains(false, None).await
    }
    async fn with_domains(
        enabled: bool,
        model: Option<Arc<dyn assistant_runtime::ModelServiceFactory>>,
    ) -> Self {
        let mock = Arc::new(Mock::default());
        let center = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let center_url = format!("http://{}", center.local_addr().unwrap());
        let center_router = Router::new()
            .route(
                "/api/info",
                get(|| async { Json(json!({"protocol_version":1,"min_protocol_version":1,"capabilities":["managed_models","llm_proxy"]})) }),
            )
            .route("/api/runtime/model-configuration", get(runtime::models::configuration))
            .route("/api/llm/providers/{provider}/v1/chat/completions", post(runtime::models::proxy))
            .route("/api/llm/providers/{provider}/v1/responses", post(runtime::models::proxy))
            .route("/api/auth/login", post(mock_login))
            .route("/api/auth/me", get(mock_me))
            .route("/api/auth/logout", post(mock_logout))
            .route(
                "/api/auth/password",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .with_state(mock.clone());
        let center_task = tokio::spawn(async move {
            axum::serve(center, center_router).await.unwrap();
        });
        let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = host.local_addr().unwrap();
        let url = format!("http://{address}");
        let test_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&test_root).unwrap();
        let home = tempfile::Builder::new()
            .prefix("c04-host-")
            .tempdir_in(test_root.canonicalize().unwrap())
            .unwrap();
        std::fs::write(home.path().join("host.toml"), format!("version='0.27.0'\nmode='enterprise'\n[enterprise]\ncenter_url='{center_url}'\n[host_access]\nport={}\nremote_enabled=true\n", address.port())).unwrap();
        let source = Arc::new(LocalConfigSource::new(home.path().join("host.toml")));
        let (mut service, access) = HostAccessService::new(source);
        service.prepare().await.unwrap();
        let shutdown = CancellationToken::new();
        let terminals = crate::user_terminal::UserTerminalService::new();
        let mut state = HttpState::starting(
            HttpEndpointState::new(
                "bootstrap-secret",
                address.to_string(),
                url.clone(),
                "test-instance".into(),
            ),
            shutdown.clone(),
            access,
            terminals.handle.clone(),
        );
        let domain_task = if enabled {
            for id in [1, 2] {
                let user = home.path().join("users").join(format!("{CENTER_ID}_{id}"));
                std::fs::create_dir_all(&user).unwrap();
                std::fs::write(user.join("config.toml"), "schema_version=1\n").unwrap();
            }
            let (mut domains, handle) = crate::user_domain::UserDomainService::new(
                crate::config::ServeConfig {
                    runtime_home: home.path().to_owned(),
                    config_path: home.path().join("host.toml"),
                    event_capacity: 64.try_into().unwrap(),
                    password_stdin: false,
                },
                state.access.credentials.clone(),
                state.startup.clone(),
                true,
            );
            domains.shared.protected_files = state.access.protected_files.clone();
            domains.model = model;
            state.domains = Some(handle);
            state.startup.identity_ready();
            let stop = shutdown.clone();
            Some(tokio::spawn(async move {
                domains.run_until(stop).await.unwrap();
            }))
        } else {
            None
        };
        let terminal_shutdown = shutdown.clone();
        let terminal_task = tokio::spawn(async move {
            terminals.run_until(terminal_shutdown).await.unwrap();
        });
        let service_state = state.clone();
        let stop = shutdown.clone();
        let service_task = tokio::spawn(async move {
            service.run_until(service_state, stop).await.unwrap();
        });
        let router = crate::http::router(state.clone());
        let host_task = tokio::spawn(async move {
            axum::serve(
                host,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            home,
            state,
            mock,
            url,
            client: reqwest::Client::new(),
            shutdown,
            tasks: vec![center_task, service_task, host_task, terminal_task],
            domain_task,
        }
    }
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.url))
            .header(
                assistant_protocol::CLIENT_VERSION_HEADER,
                env!("CARGO_PKG_VERSION"),
            )
            .header(
                assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
                assistant_protocol::MIN_COMPATIBLE_VERSION,
            );
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request
    }
    async fn login(&self, username: &str) -> String {
        let response = self.request(reqwest::Method::POST, "/auth/login", None)
            .json(&json!({"method":"enterprise","username":username,"password":"password1","native":true})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value = response.json::<Value>().await.unwrap();
        assert_eq!(value["identity"]["center_id"], CENTER_ID);
        assert!(!value.to_string().contains("ct_"));
        assert!(!value.to_string().contains("cl_"));
        value["token"].as_str().unwrap().into()
    }
    async fn session(&self, token: &str) -> reqwest::Response {
        self.request(reqwest::Method::GET, "/auth/session", Some(token))
            .send()
            .await
            .unwrap()
    }
    fn permit(&self, token: &str) -> AccessPermit {
        AccessPermit::new(
            false,
            Some(self.state.access.credentials.authenticate(token).unwrap()),
            self.shutdown.clone(),
        )
    }
}

#[tokio::test]
async fn delayed_center_logout_cannot_remove_a_subsequent_login() {
    let f = Fixture::new().await;
    let previous = f.login("alice").await;
    let held = Arc::new(Held::default());
    *f.mock.logout_hold.lock().unwrap() = Some(held.clone());
    let request = f.request(reqwest::Method::POST, "/auth/logout", Some(&previous));
    let logout = tokio::spawn(async move { request.send().await.unwrap() });
    held.started.notified().await;
    assert!(f.state.access.credentials.authenticate(&previous).is_none());
    let latest = f.login("alice").await;
    held.release.notify_one();
    assert_eq!(logout.await.unwrap().status(), StatusCode::OK);
    assert_eq!(f.session(&latest).await.status(), StatusCode::OK);
}
#[tokio::test]
async fn all_clients_use_latest_credentials_and_logout_only_ends_that_user() {
    let f = Fixture::new().await;
    let first = f.login("alice").await;
    let bob = f.login("bob").await;
    let second = f.login("alice").await;
    assert_eq!(f.mock.logout_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.session(&first).await.status(), StatusCode::OK);
    assert_eq!(
        f.mock.seen.lock().unwrap().last().unwrap(),
        &format!("ct_{:064x}", 3)
    );
    let failed = f
        .request(reqwest::Method::POST, "/auth/login", None)
        .json(&json!({"method":"enterprise","username":"alice","password":"wrong","native":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(f.session(&first).await.status(), StatusCode::OK);
    assert_eq!(
        f.request(reqwest::Method::POST, "/auth/password", Some(&first))
            .json(&json!({"old_password":"password1","new_password":"new123"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(f.session(&second).await.status(), StatusCode::OK);
    let logout = f
        .request(reqwest::Method::POST, "/auth/logout", Some(&first))
        .send()
        .await
        .unwrap();
    assert_eq!(
        logout.json::<Value>().await.unwrap(),
        json!({"local_ended":true,"center_revocation_confirmed":true})
    );
    assert_eq!(f.session(&first).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(f.session(&second).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(f.session(&bob).await.status(), StatusCode::OK);
    assert!(!f.home.path().join("users/_personal/data").exists());
}

#[tokio::test]
async fn bootstrap_cannot_impersonate_user_and_cookie_exchange_keeps_identity() {
    let f = Fixture::new().await;
    let capabilities = f
        .request(reqwest::Method::GET, "/capabilities", None)
        .send()
        .await
        .unwrap();
    assert_eq!(capabilities.status(), StatusCode::OK);
    let value = capabilities.json::<Value>().await.unwrap();
    assert_eq!(value["mode"], "enterprise");
    assert!(value.get("platform").is_none());
    let denied = f
        .request(
            reqwest::Method::POST,
            "/auth/login",
            Some("bootstrap-secret"),
        )
        .json(&json!({"method":"desktop"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let denied = f.request(reqwest::Method::POST, "/commands", Some("bootstrap-secret")).json(&json!({"request_id":"x","command":{"scope":"runtime","payload":{"type":"list_sessions","payload":{}}}})).send().await.unwrap();
    assert_ne!(denied.status(), StatusCode::OK);
    let token = f.login("alice").await;
    let exchange = f
        .request(reqwest::Method::POST, "/auth/login", None)
        .header("origin", &f.url)
        .json(&json!({"method":"token","token":token}))
        .send()
        .await
        .unwrap();
    assert_eq!(exchange.status(), StatusCode::OK);
    let cookie = exchange.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(cookie.contains("HttpOnly"));
    let value = exchange.json::<Value>().await.unwrap();
    assert!(value["token"].is_null());
    assert_eq!(value["identity"]["user_id"], 1);
    let stale = f
        .request(reqwest::Method::POST, "/auth/logout", None)
        .header("origin", &f.url)
        .header("cookie", cookie.split(';').next().unwrap())
        .header("x-ez-login-context", "old-page")
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(f.session(&token).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn bootstrap_can_query_host_identity_but_cannot_enter_any_user_route_group() {
    use reqwest::Method;
    let f = Fixture::new().await;
    assert_eq!(f.session("bootstrap-secret").await.status(), StatusCode::OK);
    // 覆盖各组的真实 HTTP 入口；无需数据库或资源存在，就必须先拒绝管理凭据。
    for (method, path) in [
        (Method::POST, "/auth/logout"),
        (Method::POST, "/auth/password"),
        (Method::POST, "/runtime/ensure-ready"),
        (Method::GET, "/events"),
        (Method::POST, "/host-files/list"),
        (Method::GET, "/sessions/s/attachments/a/preview"),
        (Method::POST, "/sessions/s/resource-files/native-path"),
        (Method::GET, "/user-terminals/socket"),
    ] {
        let response = f
            .request(method, path, Some("bootstrap-secret"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    assert!(f.mock.seen.lock().unwrap().is_empty());
    assert!(!f.home.path().join("users").exists());
}

#[tokio::test]
async fn unavailable_retains_tokens_but_explicit_invalidity_ends_user_and_logout_works_offline() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let mut permit = f.permit(&token);
    f.state
        .access
        .credentials
        .verify_enterprise(&mut permit)
        .await
        .unwrap();
    f.mock.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        f.session(&token).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(permit.check().is_err());
    assert!(f.state.access.credentials.authenticate(&token).is_some());
    f.mock.unavailable.store(false, Ordering::SeqCst);
    assert_eq!(f.session(&token).await.status(), StatusCode::OK);
    f.mock.tokens.lock().unwrap().clear();
    assert_eq!(f.session(&token).await.status(), StatusCode::UNAUTHORIZED);
    let new_token = f.login("alice").await;
    f.mock.unavailable.store(true, Ordering::SeqCst);
    // 中心离线且客户端没有版本声明时，仍要完成本机退出并送达结果。
    let ended = f
        .client
        .post(format!("{}/auth/logout", f.url))
        .bearer_auth(&new_token)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        ended,
        json!({"local_ended":true,"center_revocation_confirmed":false})
    );
    assert!(
        f.state
            .access
            .credentials
            .authenticate(&new_token)
            .is_none()
    );
}

#[tokio::test]
async fn late_failed_check_cannot_clear_new_login_and_concurrent_checks_share_request() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    f.mock.tokens.lock().unwrap().clear();
    let held = Arc::new(Held::default());
    *f.mock.hold.lock().unwrap() = Some(held.clone());
    let credentials = f.state.access.credentials.clone();
    let mut first = f.permit(&token);
    let checking = tokio::spawn(async move { credentials.verify_enterprise(&mut first).await });
    held.started.notified().await;
    let fresh = f.login("alice").await;
    held.release.notify_one();
    assert!(checking.await.unwrap().is_ok());
    assert_eq!(f.session(&fresh).await.status(), StatusCode::OK);
    let held = Arc::new(Held::default());
    *f.mock.hold.lock().unwrap() = Some(held.clone());
    let credentials = f.state.access.credentials.clone();
    let mut a = f.permit(&fresh);
    let mut b = f.permit(&fresh);
    let checks = async {
        tokio::join!(
            credentials.verify_enterprise(&mut a),
            credentials.verify_enterprise(&mut b)
        )
    };
    let release = async {
        held.started.notified().await;
        held.release.notify_one();
    };
    let before = f.mock.seen.lock().unwrap().len();
    let ((a, b), _) = tokio::join!(checks, release);
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(f.mock.seen.lock().unwrap().len(), before + 1);
}

#[tokio::test]
async fn center_binding_mismatch_rejects_login_and_revokes_only_unpublished_credentials() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let saved = std::fs::read_to_string(f.home.path().join("host.toml")).unwrap();
    assert!(saved.contains(CENTER_ID));
    f.mock.mismatch.store(true, Ordering::SeqCst);
    let client = f.state.access.center().unwrap();
    let result = f
        .state
        .access
        .credentials
        .enterprise_login(
            &client,
            &f.state.access,
            "alice".into(),
            SecretValue::new("password1".into()),
            ClientCompatibility::current(),
            &f.shutdown,
        )
        .await;
    assert!(matches!(
        result,
        Err(AccessError::Center(CenterError::IdentityMismatch))
    ));
    assert_eq!(f.mock.logout_calls.load(Ordering::SeqCst), 1);
    f.mock.mismatch.store(false, Ordering::SeqCst);
    assert_eq!(f.session(&token).await.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(f.home.path().join("host.toml")).unwrap(),
        saved
    );
}

#[tokio::test]
async fn stalled_identity_check_is_bounded_and_does_not_revoke_credentials() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let mut old = f.permit(&token);
    f.state
        .access
        .credentials
        .verify_enterprise(&mut old)
        .await
        .unwrap();
    let held = Arc::new(Held::default());
    *f.mock.hold.lock().unwrap() = Some(held.clone());
    let credentials = f.state.access.credentials.clone();
    let mut permit = f.permit(&token);
    let checking = tokio::spawn(async move { credentials.verify_enterprise(&mut permit).await });
    held.started.notified().await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(6)).await;
    assert!(matches!(
        checking.await.unwrap(),
        Err(AccessError::Center(CenterError::Unavailable))
    ));
    tokio::time::resume();
    assert!(old.check().is_err());
    assert!(f.state.access.credentials.authenticate(&token).is_some());
    held.release.notify_one();
    assert_eq!(f.session(&token).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn host_token_expiry_does_not_discard_current_center_credentials() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(7 * 24 * 60 * 60)).await;
    tokio::time::resume();
    assert!(f.state.access.credentials.authenticate(&token).is_none());
    assert_eq!(
        f.state.access.credentials.state.lock().unwrap().users.len(),
        1
    );
    assert_eq!(f.mock.logout_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn supervisor_rechecks_active_access_and_ends_it_on_center_revocation() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let mut permit = f.permit(&token);
    f.state
        .access
        .credentials
        .verify_enterprise(&mut permit)
        .await
        .unwrap();
    f.mock.tokens.lock().unwrap().clear();
    let credentials = f.state.access.credentials.clone();
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let task = tokio::spawn(async move { credentials.recheck_connections(stop).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), permit.ended())
        .await
        .unwrap();
    assert!(f.state.access.credentials.authenticate(&token).is_none());
    assert_eq!(f.mock.seen.lock().unwrap().len(), 2);
    shutdown.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn login_capacity_failure_keeps_current_credentials_and_cleans_new_center_login() {
    let f = Fixture::new().await;
    let token = f.login("alice").await;
    let permit = f.permit(&token);
    for _ in 1..64 {
        f.state
            .access
            .credentials
            .issue(&permit, ClientCompatibility::current())
            .unwrap();
    }
    let response = f
        .request(reqwest::Method::POST, "/auth/login", None)
        .json(
            &json!({"method":"enterprise","username":"alice","password":"password1","native":true}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(f.mock.logout_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.session(&token).await.status(), StatusCode::OK);
    assert_eq!(
        f.mock.seen.lock().unwrap().last().unwrap(),
        &format!("ct_{:064x}", 1)
    );
}
