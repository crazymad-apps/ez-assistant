//! 用独立目录和真实 Host 进程验证客户端接入，不读取已安装应用的数据。
#![cfg(unix)]

mod support;

use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use serde_json::{Value, json};
use std::{net::TcpListener, time::Duration};
use support::HostProcess;

fn client() -> Client {
    Client::builder()
        .default_headers(support::compatibility_headers())
        .timeout(Duration::from_secs(12))
        .no_proxy()
        .resolve("runtime.test", "127.0.0.1:0".parse().unwrap())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn command(http: &Client, base: &str, token: &str, payload: Value) -> Response {
    http.post(format!("{base}/commands")).bearer_auth(token)
        .json(&json!({"request_id":"access-test", "command":{"scope":"host_access", "payload":payload}})).send().unwrap()
}

fn status(http: &Client, host: &HostProcess) -> Value {
    let response = command(
        http,
        host.base_url(),
        host.access_token(),
        json!({"type":"get_status"}),
    );
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().unwrap()["result"]["payload"].clone()
}

fn password(http: &Client, host: &HostProcess, password: &str) {
    let current = status(http, host);
    let response = command(
        http,
        host.base_url(),
        host.access_token(),
        json!({"type":"set_password", "payload":{"expected_revision":current["revision"], "password":password}}),
    );
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().unwrap()
    );
}

fn configure(http: &Client, host: &HostProcess, configuration: Value) -> Response {
    let current = status(http, host);
    command(
        http,
        host.base_url(),
        host.access_token(),
        json!({"type":"configure", "payload":{"expected_revision":current["revision"], "configuration":configuration}}),
    )
}

fn external_configuration(host: &HostProcess) -> (String, Value) {
    let port = reqwest::Url::parse(host.base_url())
        .unwrap()
        .port()
        .unwrap();
    let origin = format!("http://runtime.test:{port}");
    (
        origin,
        json!({"remote_enabled":true, "scheme":"http", "port":port, "server_names":["runtime.test"], "tls_certificate":null, "tls_private_key":null}),
    )
}

fn login(http: &Client, base: &str, password: &str) -> String {
    let response = http
        .post(format!("{base}/auth/login"))
        .json(&json!({"method":"password", "password":password, "native":true}))
        .send()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn two_hosts_isolate_tokens_and_passwords_and_restrict_native_bootstrap() {
    let directory_a = support::test_directory();
    let directory_b = support::test_directory();
    let a = HostProcess::start(directory_a.path());
    let b = HostProcess::start(directory_b.path());
    let http = client();
    let (external, mut configuration) = external_configuration(&a);
    assert_eq!(
        configure(&http, &a, configuration.clone()).status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(status(&http, &a)["listener_state"], "closed");
    password(&http, &a, "test A password");
    password(&http, &b, "test B password");
    assert_eq!(
        configure(&http, &a, configuration.clone()).status(),
        StatusCode::OK
    );
    assert_eq!(status(&http, &a)["listener_state"], "listening");
    let token = login(&http, &external, "test A password");
    for base in [&external, a.base_url()] {
        assert_eq!(
            http.get(format!("{base}/health")).send().unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            http.get(format!("{base}/health"))
                .bearer_auth(&token)
                .send()
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(a.access_token())
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{}/health", a.base_url()))
            .bearer_auth(a.access_token())
            .header("Origin", a.base_url())
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        http.get(format!("{}/health", b.base_url()))
            .bearer_auth(&token)
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(&token)
            .header("Host", "evil.invalid")
            .send()
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(&token)
            .header("Origin", "http://evil.invalid")
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let shutdown = http.post(format!("{external}/commands")).bearer_auth(&token).json(&json!({"request_id":"forbidden-shutdown", "command":{"scope":"runtime", "payload":{"type":"shutdown_runtime", "payload":{}}}})).send().unwrap();
    assert_eq!(shutdown.status(), StatusCode::FORBIDDEN);
    configuration["port"] = json!(8089);
    assert_eq!(
        configure(&http, &a, configuration).status(),
        StatusCode::BAD_REQUEST
    );
    password(&http, &a, "changed password");
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(&token)
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{}/health", a.base_url()))
            .bearer_auth(a.access_token())
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(status(&http, &b)["listener_state"], "closed");
    let document = std::fs::read_to_string(directory_a.path().join("config.toml")).unwrap();
    assert!(document.contains("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(!document.contains("changed password"));
}

#[test]
fn web_cookie_quick_login_revocation_and_listener_close_have_separate_lifecycles() {
    use std::io::Read as _;
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let http = client();
    password(&http, &host, "cookie password");
    let (external, mut configuration) = external_configuration(&host);
    assert_eq!(
        configure(&http, &host, configuration.clone()).status(),
        StatusCode::OK
    );
    let desktop = login(&http, &external, "cookie password");
    let quick = http
        .post(format!("{external}/auth/login"))
        .bearer_auth(&desktop)
        .json(&json!({"method":"desktop"}))
        .send()
        .unwrap();
    assert_eq!(quick.status(), StatusCode::OK);
    let quick_token = quick.json::<Value>().unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(desktop, quick_token);
    let browser = http
        .post(format!("{external}/auth/login"))
        .header("Origin", &external)
        .json(&json!({"method":"token", "token":quick_token}))
        .send()
        .unwrap();
    assert_eq!(browser.status(), StatusCode::OK);
    let set_cookie = browser.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(set_cookie.contains("HttpOnly; SameSite=Strict; Path=/"));
    assert!(!set_cookie.contains("Secure"));
    let cookie = set_cookie.split(';').next().unwrap();
    assert!(browser.json::<Value>().unwrap()["token"].is_null());
    let mut events = http
        .get(format!("{external}/events"))
        .header("Cookie", cookie)
        .send()
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let mut initial = [0_u8; 1];
    events.read_exact(&mut initial).unwrap();
    assert_eq!(
        http.post(format!("{external}/auth/logout"))
            .header("Cookie", cookie)
            .send()
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(&desktop)
            .header("Cookie", cookie)
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        http.post(format!("{external}/auth/logout"))
            .header("Cookie", cookie)
            .header("Origin", &external)
            .send()
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let mut rest = Vec::new();
    let closed_at = std::time::Instant::now();
    let _ = events.read_to_end(&mut rest);
    assert!(
        closed_at.elapsed() < Duration::from_secs(2),
        "SSE must end on logout, not client timeout"
    );
    assert_eq!(
        http.get(format!("{external}/health"))
            .header("Cookie", cookie)
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(&desktop)
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    configuration["remote_enabled"] = json!(false);
    assert_eq!(
        configure(&http, &host, configuration).status(),
        StatusCode::OK
    );
    assert_eq!(status(&http, &host)["listener_state"], "closed");
    assert_eq!(
        http.get(format!("{external}/health"))
            .bearer_auth(&desktop)
            .send()
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        http.get(format!("{}/health", host.base_url()))
            .bearer_auth(host.access_token())
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[test]
fn https_uses_the_same_port_after_restart_and_requires_certificate_trust() {
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let http = client();
    password(&http, &host, "https password");
    let mut configuration = status(&http, &host)["configuration"].clone();
    let origin = host.base_url().replacen("http:", "https:", 1);
    configuration["scheme"] = json!("https");
    configuration["tls_certificate"] = json!(directory.path().join("server.crt"));
    configuration["tls_private_key"] = json!(directory.path().join("server.key"));
    assert_eq!(
        configure(&http, &host, configuration.clone()).status(),
        StatusCode::BAD_REQUEST
    );
    let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let certificate = certified.cert.pem();
    std::fs::write(directory.path().join("server.crt"), &certificate).unwrap();
    std::fs::write(
        directory.path().join("server.key"),
        certified.signing_key.serialize_pem(),
    )
    .unwrap();
    assert_eq!(
        configure(&http, &host, configuration).status(),
        StatusCode::OK
    );
    assert_eq!(status(&http, &host)["restart_required"], true);
    // 当前 HTTP 入口继续工作；重启后才整体使用 HTTPS，没有旁路 HTTP 端口。
    assert_eq!(
        http.get(format!("{}/health", host.base_url()))
            .bearer_auth(host.access_token())
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(host);
    let trusted = Client::builder()
        .default_headers(support::compatibility_headers())
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(certificate.as_bytes()).unwrap())
        .build()
        .unwrap();
    let host = HostProcess::start_with_client(directory.path(), &trusted);
    assert_eq!(host.base_url(), origin);
    assert!(
        !status(&trusted, &host)["restart_required"]
            .as_bool()
            .unwrap()
    );
    assert!(http.get(format!("{origin}/health")).send().is_err());
    let browser = trusted
        .post(format!("{origin}/auth/login"))
        .header("Origin", &origin)
        .json(&json!({"method":"password", "password":"https password", "native":false}))
        .send()
        .unwrap();
    assert_eq!(browser.status(), StatusCode::OK);
    let cookie = browser.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.starts_with("ez_host_session_https_"));
    assert!(cookie.contains("; Secure"));
}

#[test]
fn port_change_is_explicit_survives_restart_and_does_not_fall_back_on_conflict() {
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let http = client();
    password(&http, &host, "port password");
    let mut configuration = status(&http, &host)["configuration"].clone();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let new_port = reservation.local_addr().unwrap().port();
    configuration["port"] = json!(new_port);
    let denied = configure(&http, &host, configuration.clone());
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    assert!(denied.text().unwrap().contains("已被占用"));
    assert_eq!(status(&http, &host)["restart_required"], false);
    drop(reservation);
    assert_eq!(
        configure(&http, &host, configuration.clone()).status(),
        StatusCode::OK
    );
    assert_eq!(status(&http, &host)["restart_required"], true);
    configuration["remote_enabled"] = json!(true);
    assert_eq!(
        configure(&http, &host, configuration).status(),
        StatusCode::BAD_REQUEST
    );
    drop(host);
    let host = HostProcess::start(directory.path());
    assert_eq!(host.base_url(), format!("http://127.0.0.1:{new_port}"));
    assert_eq!(status(&http, &host)["restart_required"], false);
}

#[test]
fn closing_external_access_does_not_cancel_an_accepted_runtime_run() {
    let directory = support::test_directory();
    let provider = support::FakeProvider::start();
    support::write_config(
        directory.path(),
        provider.endpoint(),
        "isolated-fake-provider-key",
    );
    let host = HostProcess::start(directory.path());
    let http = client();
    password(&http, &host, "run password");
    let (origin, mut configuration) = external_configuration(&host);
    assert_eq!(
        configure(&http, &host, configuration.clone()).status(),
        StatusCode::OK
    );
    let token = login(&http, &origin, "run password");
    let mut local = host.connect();
    let session = local.runtime(
        "create_session",
        json!({"title":"remote lifecycle", "model_selection":null}),
    );
    let session_id = session["session"]["session_id"].as_str().unwrap();
    let accepted = http.post(format!("{origin}/commands")).bearer_auth(&token).json(&json!({
        "request_id":"run-before-close", "command":{"scope":"runtime", "payload":{"type":"submit_input", "payload":{
            "session_id":session_id, "idempotency_key":"independent-run", "message":"BLOCK_FOR_RESTART", "variant":"build"
        }}}
    })).send().unwrap();
    assert_eq!(
        accepted.status(),
        StatusCode::OK,
        "{}",
        accepted.text().unwrap()
    );
    let result = accepted.json::<Value>().unwrap();
    let run_id = result["result"]["payload"]["payload"]["run"]["run_id"]
        .as_str()
        .unwrap();
    local.wait_for_status(
        "running before closing listener",
        session_id,
        run_id,
        &["running"],
    );
    configuration["remote_enabled"] = json!(false);
    assert_eq!(
        configure(&http, &host, configuration).status(),
        StatusCode::OK
    );
    assert_eq!(
        local.wait_for_status(
            "Run survives listener close",
            session_id,
            run_id,
            &["completed"]
        )["status"],
        "completed"
    );
}

#[test]
fn stdin_initialization_holds_instance_lock_and_password_changes_invalidate_old_sessions() {
    use std::process::Command;
    let directory = support::test_directory();
    let host = HostProcess::start_with_password(directory.path(), " stdin password \r\n");
    let http = client();
    assert!(
        status(&http, &host)["password_configured"]
            .as_bool()
            .unwrap()
    );
    let token = login(&http, host.base_url(), " stdin password ");
    let before = std::fs::read(directory.path().join("config.toml")).unwrap();
    let second = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
        .args(["serve", "--password-stdin", "--runtime-home"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert_eq!(
        std::fs::read(directory.path().join("config.toml")).unwrap(),
        before
    );
    drop(host);
    let restarted = HostProcess::start(directory.path());
    assert_eq!(
        http.get(format!("{}/health", restarted.base_url()))
            .bearer_auth(token)
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(!login(&http, restarted.base_url(), " stdin password ").is_empty());
}

#[test]
fn browser_logins_on_two_host_ports_do_not_replace_or_logout_each_other() {
    let directory_a = support::test_directory();
    let directory_b = support::test_directory();
    let a = HostProcess::start(directory_a.path());
    let b = HostProcess::start(directory_b.path());
    let http = client();
    let browser_login =
        |host: &HostProcess| {
            password(&http, host, "port fixture password");
            let response = http.post(format!("{}/auth/login", host.base_url()))
            .header("Origin", host.base_url())
            .json(&json!({"method":"password", "password":"port fixture password", "native":false}))
            .send().unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        };
    let cookie_a = browser_login(&a);
    let cookie_b = browser_login(&b);
    assert_ne!(cookie_a.split('=').next(), cookie_b.split('=').next());
    let combined = format!("{cookie_a}; {cookie_b}");
    for host in [&a, &b] {
        assert_eq!(
            http.get(format!("{}/auth/session", host.base_url()))
                .header("Cookie", &combined)
                .send()
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    let logout = http
        .post(format!("{}/auth/logout", a.base_url()))
        .header("Origin", a.base_url())
        .header("Cookie", &combined)
        .send()
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert!(
        logout.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .starts_with(cookie_a.split('=').next().unwrap())
    );
    assert_eq!(
        http.get(format!("{}/auth/session", b.base_url()))
            .header("Cookie", &cookie_b)
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[test]
fn occupied_configured_port_prevents_startup_without_publishing_another_port() {
    let directory = support::test_directory();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let configuration = assistant_protocol::HostAccessConfiguration {
        port: occupied.local_addr().unwrap().port(),
        ..Default::default()
    };
    std::fs::write(
        directory.path().join("config.toml"),
        format!(
            "schema_version = 1\ndefault_model = \"\"\n[host_access]\n{}",
            toml::to_string(&configuration).unwrap()
        ),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
        .args(["serve", "--runtime-home"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("已被占用"));
    assert!(!directory.path().join("run/runtime.json").exists());
    assert_eq!(
        assistant_protocol::HostAccessConfiguration::default().port,
        7240
    );
}

#[test]
fn bearer_access_is_origin_independent_while_cookie_access_stays_same_origin() {
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let http = client();
    password(&http, &host, "unified access password");
    let base = host.base_url();
    let response = http
        .post(format!("{base}/auth/login"))
        .header("Origin", "http://localhost:1420")
        .json(&json!({"method":"password", "password":"unified access password", "native":true}))
        .send()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["access-control-allow-origin"], "*");
    assert!(
        !response
            .headers()
            .contains_key("access-control-allow-credentials")
    );
    let token = response.json::<Value>().unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    for origin in [
        "http://localhost:1420",
        "tauri://localhost",
        "https://other.test",
        "null",
    ] {
        let preflight = http
            .request(reqwest::Method::OPTIONS, format!("{base}/health"))
            .header("Origin", origin)
            .header("Access-Control-Request-Method", "GET")
            .header(
                "Access-Control-Request-Headers",
                "authorization,x-ez-client-version",
            )
            .send()
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(preflight.headers()["access-control-allow-origin"], "*");
        assert_eq!(
            http.get(format!("{base}/health"))
                .header("Origin", origin)
                .bearer_auth(&token)
                .send()
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let denied = http
            .get(format!("{base}/health"))
            .header("Origin", origin)
            .send()
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(denied.headers()["access-control-allow-origin"], "*");
        let invalid = http
            .get(format!("{base}/health"))
            .header("Origin", origin)
            .bearer_auth("invalid")
            .send()
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(invalid.headers()["access-control-allow-origin"], "*");
    }
    for origin in [None, Some("http://localhost:1420"), Some("null")] {
        let request = http.post(format!("{base}/auth/login")).json(
            &json!({"method":"password", "password":"unified access password", "native":false}),
        );
        let request = if let Some(origin) = origin {
            request.header("Origin", origin)
        } else {
            request
        };
        let response = request.send().unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key("set-cookie"));
    }
    let browser = http
        .post(format!("{base}/auth/login"))
        .header("Origin", base)
        .json(&json!({"method":"password", "password":"unified access password", "native":false}))
        .send()
        .unwrap();
    assert_eq!(browser.status(), StatusCode::OK);
    let cookie = browser.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for (method, path) in [
        (reqwest::Method::GET, "health"),
        (reqwest::Method::POST, "auth/logout"),
        (reqwest::Method::GET, "user-terminals/socket"),
    ] {
        assert_eq!(
            http.request(method, format!("{base}/{path}"))
                .header("Cookie", &cookie)
                .header("Origin", "http://localhost:1420")
                .send()
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        http.get(format!("{base}/health"))
            .header("Cookie", &cookie)
            .header("Origin", base)
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        http.get(format!("{base}/health"))
            .header("Cookie", &cookie)
            .bearer_auth("invalid")
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{base}/health"))
            .header("Cookie", &cookie)
            .header("Origin", "http://localhost:1420")
            .bearer_auth(&token)
            .send()
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let shutdown = http.post(format!("{base}/commands")).bearer_auth(&token)
        .header("Origin", "tauri://localhost")
        .json(&json!({"request_id":"no-origin-privilege", "command":{"scope":"runtime", "payload":{"type":"shutdown_runtime", "payload":{}}}}))
        .send().unwrap();
    assert_eq!(shutdown.status(), StatusCode::FORBIDDEN);
}
