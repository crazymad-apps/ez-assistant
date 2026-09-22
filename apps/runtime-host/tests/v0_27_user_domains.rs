//! 正式 Host 进程的企业装配与 HTTP/SSE 路由；所有目录、Center 与 SQLite 均为本次新建夹具。
use axum::{
    Json, Router,
    extract::Json as RequestJson,
    http::HeaderMap,
    routing::{get, post},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

const CENTER: &str = "01234567-89ab-4cde-8f01-23456789abcd";
struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn identity(id: u64) -> Value {
    json!({"center_id": CENTER, "user": {"id": id, "username": format!("user{id}"), "display_name": format!("User {id}"), "role":"user", "is_super_admin":false, "enabled":true}})
}
fn request(
    client: &reqwest::Client,
    url: &str,
    path: &str,
    token: &str,
) -> reqwest::RequestBuilder {
    let request = client.post(format!("{url}{path}"));
    let request = if token.is_empty() {
        request
    } else {
        request.bearer_auth(token)
    };
    request
        .header(
            assistant_protocol::CLIENT_VERSION_HEADER,
            assistant_protocol::SOFTWARE_VERSION,
        )
        .header(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            assistant_protocol::MIN_COMPATIBLE_VERSION,
        )
}
async fn login(client: &reqwest::Client, url: &str, user: &str) -> String {
    let response = request(client, url, "/auth/login", "")
        .json(&json!({"method":"enterprise","username":user,"password":"fixture123","native":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .into()
}
async fn create_session(client: &reqwest::Client, url: &str, token: &str, title: &str) -> String {
    let response = request(client, url, "/commands", token).json(&json!({"request_id":"create","command":{"scope":"runtime","payload":{"type":"create_session","payload":{"title":title,"model_selection":null}}}})).send().await.unwrap();
    let status = response.status();
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body["result"]["payload"]["payload"]["session"]["session_id"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
async fn real_process_has_no_personal_fallback_and_routes_events_to_its_user() {
    let center = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let center_url = format!("http://{}", center.local_addr().unwrap());
    let center_router = Router::new()
        .route(
            "/api/info",
            get(|| async { Json(json!({"protocol_version":1,"min_protocol_version":1})) }),
        )
        .route(
            "/api/auth/login",
            post(|RequestJson(body): RequestJson<Value>| async move {
                let id = if body["username"] == "bob" { 2 } else { 1 };
                let mut result = identity(id);
                result["token"] = json!(format!("ct_{id:064x}"));
                result["llm_key"] = json!(format!("cl_{id:064x}"));
                Json(result)
            }),
        )
        .route(
            "/api/auth/me",
            get(|headers: HeaderMap| async move {
                let id = if headers["authorization"].to_str().unwrap().ends_with('2') {
                    2
                } else {
                    1
                };
                Json(identity(id))
            }),
        )
        .route(
            "/api/auth/logout",
            post(|| async { axum::http::StatusCode::NO_CONTENT }),
        );
    let center_task = tokio::spawn(async move {
        axum::serve(center, center_router).await.unwrap();
    });
    let home = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(home.path().join("host.toml"), format!("version='0.27.0'\nmode='enterprise'\n[enterprise]\ncenter_url='{center_url}'\n[host_access]\nport={port}\n")).unwrap();
    let log = std::fs::File::create(home.path().join("host.log")).unwrap();
    let mut host = Host(
        Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
            .args(["serve", "--runtime-home"])
            .arg(home.path())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                host.0.try_wait().unwrap().is_none(),
                "Host exited: {}",
                std::fs::read_to_string(home.path().join("host.log")).unwrap()
            );
            if client
                .get(format!("{url}/capabilities"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let alice = login(&client, &url, "alice").await;
    let bob = login(&client, &url, "bob").await;
    assert!(
        !home.path().join("users").exists(),
        "login must not open any user database"
    );
    let (a, b) = tokio::join!(
        request(&client, &url, "/runtime/ensure-ready", &alice).send(),
        request(&client, &url, "/runtime/ensure-ready", &bob).send()
    );
    assert_eq!(a.unwrap().status(), 204);
    assert_eq!(b.unwrap().status(), 204);
    let events = client
        .get(format!("{url}/events"))
        .bearer_auth(&alice)
        .header(
            assistant_protocol::CLIENT_VERSION_HEADER,
            assistant_protocol::SOFTWARE_VERSION,
        )
        .header(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            assistant_protocol::MIN_COMPATIBLE_VERSION,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), 200);
    let mut events = events.bytes_stream();
    let first = events.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("connected"));
    let b_id = create_session(&client, &url, &bob, "Bob secret").await;
    let a_id = create_session(&client, &url, &alice, "Alice secret").await;
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = String::new();
        loop {
            seen.push_str(&String::from_utf8_lossy(
                &events.next().await.unwrap().unwrap(),
            ));
            if seen.contains(&a_id) {
                break seen;
            }
        }
    })
    .await
    .unwrap();
    assert!(!observed.contains(&b_id));
    assert!(!observed.contains("Bob secret"));
    assert!(!home.path().join("users/_personal").exists());
    let denied = request(&client, &url, "/commands", &bob).json(&json!({"request_id":"cross","command":{"scope":"runtime","payload":{"type":"get_session_view","payload":{"session_id":a_id}}}})).send().await.unwrap();
    assert_eq!(denied.status(), 404);
    assert_eq!(
        request(&client, &url, "/auth/logout", &alice)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let latest = login(&client, &url, "alice").await;
    assert_eq!(
        request(&client, &url, "/runtime/ensure-ready", &latest)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let restored = request(&client, &url, "/commands", &latest).json(&json!({"request_id":"own","command":{"scope":"runtime","payload":{"type":"get_session_view","payload":{"session_id":a_id}}}})).send().await.unwrap();
    assert_eq!(restored.status(), 200);
    let discovery: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join("run/runtime.json")).unwrap(),
    )
    .unwrap();
    let bootstrap = discovery["access_token"].as_str().unwrap();
    assert_eq!(request(&client, &url, "/commands", bootstrap).json(&json!({"request_id":"stop","command":{"scope":"runtime","payload":{"type":"shutdown_runtime","payload":{}}}})).send().await.unwrap().status(), 200);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(status) = host.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for id in [1, 2] {
        let db = home
            .path()
            .join(format!("users/{CENTER}_{id}/data/runtime.sqlite3"));
        let connection =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2); // 既有默认主控 Session + 本测试创建的普通 Session。
        let own: String = connection
            .query_row(
                "SELECT title FROM sessions WHERE role='standard'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            own,
            if id == 1 {
                "Alice secret"
            } else {
                "Bob secret"
            }
        );
    }
    center_task.abort();
    let _ = center_task.await;
}
