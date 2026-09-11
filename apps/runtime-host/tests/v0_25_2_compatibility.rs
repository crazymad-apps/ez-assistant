//! Ready Host 上的拒绝路径；只启动隔离夹具，不修改用户数据。
#![cfg(unix)]
mod support;

use reqwest::{StatusCode, blocking::Client};
use serde_json::{Value, json};
use std::{fs, time::Duration};
use support::HostProcess;

#[test]
fn incompatible_requests_leave_the_ready_instance_running() {
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let http = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let discovery_path = directory.path().join("run/runtime.json");
    let discovery = fs::read(&discovery_path).unwrap();
    let before = database_snapshot(directory.path());
    for (version, minimum, expected) in [
        (None, None, "missing_declaration"),
        (Some("0.25.1"), Some("0.25.1"), "client_too_old"),
        (Some("0.25.3"), Some("0.25.3"), "host_too_old"),
        (Some("0.25.2"), None, "invalid_declaration"),
    ] {
        for (method, path) in [
            (reqwest::Method::POST, "/commands"),
            (reqwest::Method::GET, "/events"),
            (reqwest::Method::POST, "/sessions/missing/attachments"),
            (reqwest::Method::GET, "/sessions/missing/export.md"),
        ] {
            let mut request = http
                .request(method, format!("{}{path}", host.base_url()))
                .bearer_auth(host.access_token());
            if let Some(version) = version {
                request = request.header(assistant_protocol::CLIENT_VERSION_HEADER, version);
            }
            if let Some(minimum) = minimum {
                request =
                    request.header(assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER, minimum);
            }
            let response = request.json(&json!({"request_id":"rejected-shutdown", "command":{"scope":"runtime","payload":{"type":"shutdown_runtime","payload":{}}}})).send().unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT, "{path}");
            assert_eq!(response.json::<Value>().unwrap()["error"]["code"], expected);
        }
    }
    for version in ["0.25.2", "0.25.3"] {
        assert!(
            http.get(format!("{}/auth/session", host.base_url()))
                .bearer_auth(host.access_token())
                .header(assistant_protocol::CLIENT_VERSION_HEADER, version)
                .header(assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER, "0.25.2")
                .send()
                .unwrap()
                .status()
                .is_success()
        );
    }
    assert_eq!(fs::read(discovery_path).unwrap(), discovery);
    let health: Value = http
        .get(format!("{}/health", host.base_url()))
        .bearer_auth(host.access_token())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["status"], "ready");
    assert_eq!(database_snapshot(directory.path()), before);
}

// Only read the artificial fixture. Exact counts plus all field values prove rejection had no business write.
fn database_snapshot(
    home: &std::path::Path,
) -> std::collections::BTreeMap<String, (i64, Vec<Vec<String>>)> {
    let mut connection = rusqlite::Connection::open_with_flags(
        home.join("data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let transaction = connection.transaction().unwrap();
    let names = transaction
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut snapshot = std::collections::BTreeMap::new();
    for name in names {
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let count: i64 = transaction
            .query_row(&format!("SELECT COUNT(*) FROM {quoted}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut statement = transaction
            .prepare(&format!("SELECT * FROM {quoted}"))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get_ref(index).map(|value| format!("{value:?}")))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        snapshot.insert(name, (count, rows));
    }
    snapshot
}

#[test]
#[ignore = "显式设置 EZ_ASSISTANT_V0251_HOST 为已核验发布旧 Host；只使用隔离 Runtime Home"]
fn released_old_host_cannot_replace_the_running_instance() {
    let old_host = std::env::var_os("EZ_ASSISTANT_V0251_HOST").expect("verified old Host path");
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let discovery = fs::read(directory.path().join("run/runtime.json")).unwrap();
    let before = database_snapshot(directory.path());
    let mut child = std::process::Command::new(old_host)
        .args(["serve", "--runtime-home"])
        .arg(directory.path())
        .env("HOME", directory.path().join("fixture-user-home"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("old Host failed to reject the owned instance lock within 10 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("instance lock"));
    assert_eq!(
        fs::read(directory.path().join("run/runtime.json")).unwrap(),
        discovery
    );
    assert_eq!(database_snapshot(directory.path()), before);
    assert!(
        Client::new()
            .get(format!("{}/health", host.base_url()))
            .bearer_auth(host.access_token())
            .send()
            .unwrap()
            .status()
            .is_success()
    );
}

#[test]
fn loopback_cookie_login_media_and_events_require_the_current_page_declaration() {
    use std::io::Read as _;
    let directory = support::test_directory();
    let host = HostProcess::start(directory.path());
    let http = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let quick: Value = http
        .post(format!("{}/auth/login", host.base_url()))
        .bearer_auth(host.access_token())
        .headers(support::compatibility_headers())
        .json(&json!({"method":"desktop"}))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    let browser = http
        .post(format!("{}/auth/login", host.base_url()))
        .header("Origin", host.base_url())
        .header(assistant_protocol::CLIENT_VERSION_HEADER, "0.25.3")
        .header(assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER, "0.25.2")
        .json(&json!({"method":"token","token":quick["token"]}))
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
    assert!(browser.json::<Value>().unwrap()["token"].is_null());
    let missing = http
        .get(format!("{}/sessions/missing/export.md", host.base_url()))
        .header("Cookie", &cookie)
        .send()
        .unwrap();
    assert_eq!(
        missing.status(),
        StatusCode::NOT_FOUND,
        "media uses compatible login then resolves the missing resource"
    );
    let stale = http
        .get(format!("{}/sessions/missing/export.md", host.base_url()))
        .header("Cookie", &cookie)
        .header(assistant_protocol::CLIENT_VERSION_HEADER, "0.25.1")
        .header(assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER, "0.25.1")
        .send()
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        stale.json::<Value>().unwrap()["error"]["code"],
        "client_too_old"
    );
    let missing = http
        .get(format!("{}/events", host.base_url()))
        .header("Cookie", &cookie)
        .send()
        .unwrap();
    assert_eq!(missing.status(), StatusCode::CONFLICT);
    let mut events = http
        .get(format!("{}/events", host.base_url()))
        .header("Cookie", &cookie)
        .headers(support::compatibility_headers())
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    events.read_exact(&mut [0_u8; 1]).unwrap();
    assert_eq!(
        http.post(format!("{}/auth/logout", host.base_url()))
            .header("Cookie", &cookie)
            .header("Origin", host.base_url())
            .send()
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let stopped = std::time::Instant::now();
    let _ = events.read_to_end(&mut Vec::new());
    assert!(stopped.elapsed() < Duration::from_secs(2));
    assert_eq!(
        http.get(format!("{}/health", host.base_url()))
            .header("Cookie", &cookie)
            .send()
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(
        http.get(format!("{}/health", host.base_url()))
            .bearer_auth(host.access_token())
            .send()
            .unwrap()
            .status()
            .is_success()
    );
}
