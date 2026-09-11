//! 只在 TempDir 人工库验证监听先行、故障诊断与业务准入；不读取用户数据库。
use reqwest::{StatusCode, blocking::Client};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start(home: &Path) -> Host {
    start_binary(home, Path::new(env!("CARGO_BIN_EXE_ez-assistant-runtime")))
}
fn start_binary(home: &Path, binary: &Path) -> Host {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    fs::write(
        home.join("config.toml"),
        format!("schema_version = 1\ndefault_model = \"\"\n[host_access]\nport = {port}\n"),
    )
    .unwrap();
    Host(
        Command::new(binary)
            .args(["serve", "--runtime-home"])
            .arg(home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn http() -> Client {
    Client::builder()
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                assistant_protocol::CLIENT_VERSION_HEADER,
                reqwest::header::HeaderValue::from_static(assistant_protocol::SOFTWARE_VERSION),
            );
            headers.insert(
                assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
                reqwest::header::HeaderValue::from_static(
                    assistant_protocol::MIN_COMPATIBLE_VERSION,
                ),
            );
            headers
        })
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}
fn wait_health(home: &Path, host: &mut Host, status: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            host.0.try_wait().unwrap().is_none(),
            "Host exited before diagnostic"
        );
        if let Ok(bytes) = fs::read(home.join("run/runtime.json"))
            && let Ok(discovery) = serde_json::from_slice::<Value>(&bytes)
            && let Ok(response) = http()
                .get(format!("{}/health", discovery["address"].as_str().unwrap()))
                .bearer_auth(discovery["access_token"].as_str().unwrap())
                .send()
            && let Ok(health) = response.json::<Value>()
            && health["status"] == status
        {
            return discovery;
        }
        assert!(
            Instant::now() < deadline,
            "startup status {status} timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn command(kind: &str) -> Value {
    json!({"request_id":"startup-test","command":{"scope":"runtime","payload":{"type":kind,"payload":{}}}})
}
fn gated(discovery: &Value) {
    let client = http();
    let address = discovery["address"].as_str().unwrap();
    let token = discovery["access_token"].as_str().unwrap();
    for path in ["/health", "/capabilities"] {
        assert_eq!(
            client
                .get(format!("{address}{path}"))
                .send()
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!("{address}{path}"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    let capabilities: Value = client
        .get(format!("{address}/capabilities"))
        .bearer_auth(token)
        .send()
        .unwrap()
        .json()
        .unwrap();
    if capabilities["runtime_version"] == "0.25.1" {
        assert_eq!(capabilities["protocol_version"], 3); // 真实已发布旧程序的行为。
    } else {
        assert!(capabilities.get("protocol_version").is_none());
        assert_eq!(
            capabilities["min_compatible_version"],
            assistant_protocol::MIN_COMPATIBLE_VERSION
        );
    }
    assert!(
        capabilities["features"]
            .as_array()
            .unwrap()
            .contains(&json!("startup_diagnostics"))
    );
    assert_eq!(
        client
            .post(format!("{address}/commands"))
            .bearer_auth(token)
            .json(&command("get_application_snapshot"))
            .send()
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    for path in ["/events", "/user-terminals/socket"] {
        assert_eq!(
            client
                .get(format!("{address}{path}"))
                .bearer_auth(token)
                .send()
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    for path in [
        "/sessions/fixture/attachments",
        "/session-materializations",
        "/host-files/list",
    ] {
        assert_eq!(
            client
                .post(format!("{address}{path}"))
                .bearer_auth(token)
                .body("fixture")
                .send()
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
fn stop(discovery: &Value, host: &mut Host) {
    let response = http()
        .post(format!(
            "{}/commands",
            discovery["address"].as_str().unwrap()
        ))
        .bearer_auth(discovery["access_token"].as_str().unwrap())
        .json(&command("shutdown_runtime"))
        .send()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
}

/// 必须提供已核验来源的真实旧程序；默认测试不使用模拟程序冒充回退验收。
#[test]
#[ignore = "requires verified EZ_ASSISTANT_V0251_HOST and approval for isolated database operations"]
fn released_v0251_host_refuses_upgraded_database_without_changing_data_files() {
    let binary = std::env::var_os("EZ_ASSISTANT_V0251_HOST")
        .expect("set the verified released v0.25.1 Host path");
    let binary = Path::new(&binary);
    assert!(binary.is_absolute() && binary.is_file());
    let home = tempfile::tempdir().unwrap();
    let mut old = start_binary(home.path(), binary);
    let discovery = wait_health(home.path(), &mut old, "ready");
    let response: Value = http()
        .get(format!("{}/health", discovery["address"].as_str().unwrap()))
        .bearer_auth(discovery["access_token"].as_str().unwrap())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(response["target_version"], "0.25.1");
    stop(&discovery, &mut old);
    let database = home.path().join("data/runtime.sqlite3");
    let readonly = |path: &Path| {
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    };
    let count = |connection: &rusqlite::Connection, table: &str| -> i64 {
        connection
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{}\"", table.replace('"', "\"\"")),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = readonly(&database);
    assert_eq!(count(&before, "schema_migrations"), 1);
    assert_eq!(count(&before, "sessions"), 1);
    drop(before);

    let mut current = start(home.path());
    let discovery = wait_health(home.path(), &mut current, "ready");
    stop(&discovery, &mut current);
    let upgraded = readonly(&database);
    assert_eq!(count(&upgraded, "schema_migrations"), 2);
    assert_eq!(count(&upgraded, "sessions"), 1);
    assert_eq!(
        upgraded
            .query_row(
                "SELECT min_compatible_host_version FROM database_compatibility WHERE id=1",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "0.25.2"
    );
    let backup_dir = fs::read_dir(home.path().join("backups/database"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let backup = readonly(&backup_dir.join("runtime.sqlite3"));
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let evidence: Value =
        serde_json::from_slice(&fs::read(backup_dir.join("manifest.json")).unwrap()).unwrap();
    for (table, expected) in evidence["database"]["tables"].as_object().unwrap() {
        assert_eq!(
            count(&backup, table),
            expected["rows"].as_i64().unwrap(),
            "backup {table}"
        );
    }
    assert_eq!(count(&backup, "schema_migrations"), 1);
    assert_eq!(count(&backup, "sessions"), 1);
    drop(backup);
    drop(upgraded);
    fn files(directory: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut result = std::collections::BTreeMap::new();
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.extend(files(&path));
            } else {
                result.insert(path.clone(), fs::read(path).unwrap());
            }
        }
        result
    }
    let preserved = files(&home.path().join("data"));
    let mut old = start_binary(home.path(), binary);
    let discovery = wait_health(home.path(), &mut old, "unavailable");
    let failure: Value = http()
        .get(format!("{}/health", discovery["address"].as_str().unwrap()))
        .bearer_auth(discovery["access_token"].as_str().unwrap())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(failure["error"], "database_newer");
    gated(&discovery);
    stop(&discovery, &mut old);
    assert_eq!(files(&home.path().join("data")), preserved);
}
#[test]
fn invalid_database_keeps_authenticated_diagnostics_without_retry_or_recreation() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join("data")).unwrap();
    let path = home.path().join("data/runtime.sqlite3");
    let sentinel = b"intentional invalid SQLite fixture";
    fs::write(&path, sentinel).unwrap();
    let mut host = start(home.path());
    let discovery = wait_health(home.path(), &mut host, "unavailable");
    gated(&discovery);
    let health: Value = http()
        .get(format!("{}/health", discovery["address"].as_str().unwrap()))
        .bearer_auth(discovery["access_token"].as_str().unwrap())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["error"], "database_unavailable");
    assert!(!health.to_string().contains(home.path().to_str().unwrap()));
    assert_eq!(fs::read(&path).unwrap(), sentinel);
    stop(&discovery, &mut host);
    assert_eq!(fs::read(&path).unwrap(), sentinel);
}
#[test]
fn locked_simulated_database_publishes_starting_then_ready_on_the_same_instance() {
    let home = tempfile::tempdir().unwrap();
    let mut initial = start(home.path());
    let discovery = wait_health(home.path(), &mut initial, "ready");
    stop(&discovery, &mut initial);
    let database = rusqlite::Connection::open(home.path().join("data/runtime.sqlite3")).unwrap();
    database.execute_batch("CREATE TABLE fixture_marker(value TEXT); INSERT INTO fixture_marker VALUES('preserve'); BEGIN EXCLUSIVE").unwrap();
    let mut host = start(home.path());
    let starting = wait_health(home.path(), &mut host, "starting");
    gated(&starting);
    database.execute_batch("COMMIT").unwrap();
    let ready = wait_health(home.path(), &mut host, "ready");
    assert_eq!(starting["instance_id"], ready["instance_id"]);
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM fixture_marker WHERE value='preserve'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    stop(&ready, &mut host);
}

#[test]
fn fresh_model_schema_and_config_cleanup_survive_repeated_product_startup() {
    let home = tempfile::tempdir().unwrap();
    let mut host = start(home.path());
    let discovery = wait_health(home.path(), &mut host, "ready");
    let health: Value = http()
        .get(format!("{}/health", discovery["address"].as_str().unwrap()))
        .bearer_auth(discovery["access_token"].as_str().unwrap())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["database_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(health["min_compatible_host_version"], "0.25.2");
    let configuration = fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!configuration.contains("default_model"));
    assert!(configuration.contains("[host_access]"));
    let query = |kind: &str| -> Value {
        let response = http()
            .post(format!(
                "{}/commands",
                discovery["address"].as_str().unwrap()
            ))
            .bearer_auth(discovery["access_token"].as_str().unwrap())
            .json(&command(kind))
            .send()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json().unwrap()
    };
    let providers = query("list_providers");
    assert!(!providers.to_string().contains("api_key"));
    query("get_model_settings");
    query("get_application_snapshot");
    let database = rusqlite::Connection::open_with_flags(
        home.path().join("data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let ledger: (String, i64) = database
        .query_row(
            "SELECT version, applied_at_ms FROM schema_migrations ORDER BY version DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(ledger.0, "0.25.2");
    let count = |table: &str| {
        database
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(count("schema_migrations"), 2);
    assert_eq!(count("database_compatibility"), 1);
    assert_eq!(count("providers"), 0);
    assert_eq!(count("model_fixed_configs"), 0);
    assert_eq!(count("model_settings"), 1);
    assert_eq!(
        count("sessions"),
        1,
        "unconfigured controller can be created without a model key"
    );
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name='model_key'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(database.query_row("SELECT COUNT(*) FROM sessions WHERE model_provider_instance_id IS NULL AND model_id IS NULL", [], |row| row.get::<_,i64>(0)).unwrap(), 1);
    drop(database);
    drop(host);
    let mut host = Host(
        Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
            .args(["serve", "--runtime-home"])
            .arg(home.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_health(home.path(), &mut host, "ready");
    assert_eq!(
        fs::read_to_string(home.path().join("config.toml")).unwrap(),
        configuration
    );
    let database = rusqlite::Connection::open_with_flags(
        home.path().join("data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT version, applied_at_ms FROM schema_migrations ORDER BY version DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            )
            .unwrap(),
        ledger
    );
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn legacy_invalid_model_reference_is_discarded_after_verified_backup() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("data")).unwrap();
    let database_path = home.path().join("data/runtime.sqlite3");
    let old = rusqlite::Connection::open(&database_path).unwrap();
    old.execute_batch(include_str!(
        "../src/storage/migrations/tests/legacy_v0_25_0.sql"
    ))
    .unwrap();
    old.execute_batch("INSERT INTO sessions(session_id,title,model_key,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms) VALUES ('legacy','history preserved','invalid model !','[]','{}','archived',1,0,1,2)").unwrap();
    assert_eq!(
        old.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(old);
    let mut host = start(home.path());
    let discovery = wait_health(home.path(), &mut host, "ready");
    let upgraded = rusqlite::Connection::open_with_flags(
        &database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(upgraded.query_row("SELECT COUNT(*) FROM sessions WHERE session_id='legacy' AND title='history preserved' AND lifecycle='archived' AND body_generation=1 AND message_count=0 AND model_provider_instance_id IS NULL AND model_id IS NULL", [], |row| row.get::<_,i64>(0)).unwrap(), 1);
    let backup_dir = fs::read_dir(home.path().join("backups/database"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let backup = rusqlite::Connection::open_with_flags(
        backup_dir.join("runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        backup
            .query_row(
                "SELECT model_key FROM sessions WHERE session_id='legacy'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "invalid model !"
    );
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let response = http().post(format!("{}/commands", discovery["address"].as_str().unwrap()))
        .bearer_auth(discovery["access_token"].as_str().unwrap())
        .json(&json!({"request_id":"legacy-history","command":{"scope":"runtime","payload":{"type":"get_session","payload":{"session_id":"legacy"}}}})).send().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value = response.json().unwrap();
    let text = result.to_string();
    assert!(text.contains("history preserved"));
    assert!(!text.contains("invalid model !"));
}

#[test]
fn provider_usage_counts_unloaded_archived_sessions_and_deletion_preserves_references() {
    let home = tempfile::tempdir().unwrap();
    let mut host = start(home.path());
    wait_health(home.path(), &mut host, "ready");
    drop(host);
    let database = home.path().join("data/runtime.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;
        INSERT INTO providers VALUES ('usage-one','同名','local','http://127.0.0.1:9/v1','fixture-secret','chat_completions','/v1/models','openai');
        INSERT INTO providers VALUES ('usage-two','同名','local','http://127.0.0.1:9/v1','fixture-secret','chat_completions','/v1/models','openai');
        UPDATE model_settings SET default_provider_instance_id='usage-one',default_model_id='same/model',vision_provider_instance_id='usage-one',vision_model_id='same/model';
        INSERT INTO model_fixed_configs(provider_instance_id,model_id,context_window_tokens,max_output_tokens,max_input_tokens,mode_limits_json,capabilities_json,updated_at_ms,origin) VALUES ('usage-one','same/model',8192,4096,NULL,'{}','{}',1,'online');
        INSERT INTO model_fixed_configs(provider_instance_id,model_id,context_window_tokens,max_output_tokens,max_input_tokens,mode_limits_json,capabilities_json,updated_at_ms,origin) VALUES ('usage-two','same/model',8192,4096,NULL,'{}','{}',1,'online');").unwrap();
    for index in 0..28 {
        // 无效正文故意保留，证明统计不走 Session 装配。最后一个会话属于另一个同名实例。
        connection.execute("INSERT INTO sessions(session_id,title,model_provider_instance_id,model_id,system_prompt_json,skill_catalog_json,lifecycle,body_generation,message_count,created_at_ms,updated_at_ms)
            VALUES (?1,?2,?3,'same/model','invalid prompt fixture','invalid catalog fixture',?4,1,0,1,1)",
            rusqlite::params![format!("usage-session-{index:02}"),format!("历史会话 {index}"), if index == 27 { "usage-two" } else { "usage-one" }, if index % 2 == 0 { "archived" } else { "active" }]).unwrap();
    }
    // 独立 SQLite Backup API 副本；删除前实际连接并逐表核对精确数量。
    let backup_path = home.path().join("usage-before-delete.sqlite3");
    let mut backup = rusqlite::Connection::open(&backup_path).unwrap();
    rusqlite::backup::Backup::new(&connection, &mut backup)
        .unwrap()
        .run_to_completion(32, Duration::ZERO, None)
        .unwrap();
    for (table, expected) in [
        ("providers", 2),
        ("model_settings", 1),
        ("model_fixed_configs", 2),
        ("sessions", 29),
    ] {
        for database in [&connection, &backup] {
            let count: i64 = database
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, expected);
        }
    }
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(backup);
    drop(connection);
    let mut host = start(home.path());
    let discovery = wait_health(home.path(), &mut host, "ready");
    let invoke = |kind: &str| {
        let response = http().post(format!("{}/commands",discovery["address"].as_str().unwrap()))
            .bearer_auth(discovery["access_token"].as_str().unwrap())
            .json(&json!({"request_id":"usage-test","command":{"scope":"runtime","payload":{"type":kind,"payload":{"provider_instance_id":"usage-one"}}}}))
            .send().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.json::<Value>().unwrap()["result"]["payload"]["payload"].clone()
    };
    let usage = invoke("get_provider_usage");
    assert_eq!(usage["default_model"], true);
    assert_eq!(usage["vision_model"], true);
    assert_eq!(usage["session_count"], 27);
    assert_eq!(usage["fixed_config_count"], 1);
    assert_eq!(usage["sessions"].as_array().unwrap().len(), 20);
    assert_eq!(usage["sessions"][0]["title"], "历史会话 0");
    assert!(!usage.to_string().contains("fixture-secret"));
    assert_eq!(invoke("delete_provider"), usage);
    drop(host);
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    for (table, expected) in [
        ("providers", 1),
        ("model_settings", 1),
        ("model_fixed_configs", 1),
        ("sessions", 29),
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            expected
        );
    }
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sessions WHERE model_provider_instance_id='usage-one' AND model_id='same/model'", [], |row| row.get::<_,i64>(0)).unwrap(), 27);
    assert_eq!(connection.query_row("SELECT default_provider_instance_id,vision_provider_instance_id FROM model_settings", [], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).unwrap(), ("usage-one".into(), "usage-one".into()));
    assert_eq!(
        connection
            .query_row(
                "SELECT provider_instance_id FROM model_fixed_configs",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "usage-two"
    );
    assert!(backup_path.is_file());
}
