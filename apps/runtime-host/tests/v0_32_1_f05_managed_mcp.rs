//! 新建隔离 Home，经正式安装／配置／刷新命令核对插件 MCP 的文件与 SQL 归属。

mod support;

use std::{
    fs,
    io::Cursor,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use flate2::{Compression, write::GzEncoder};
use reqwest::blocking::Client;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use support::{FakeProvider, HostProcess};

type ObservedHeaders = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;

#[test]
fn plugin_mcp_configuration_uses_its_config_directory_through_product_commands() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f05-m0-product-")
        .tempdir_in(base)
        .unwrap();
    // 新建、无源库的本机 SQLite；应用的安装／参数写入实际落 plugins、plugin_operations。
    // 初装各一行，升级后安装操作两行；参数只改原 plugins.parameters_json。主控会话一行，输入与 Run 零行。
    let host = HostProcess::start(home.path());
    let user = home.path().join("users/_personal");
    let database = user.join("data/runtime.sqlite3");
    audit(&database, 0, 0);
    let headers = ObservedHeaders::default();
    let remote = FakeProvider::with_router(
        Router::new()
            .route("/mcp", post(mcp))
            .with_state(headers.clone()),
    );
    let endpoint = remote.endpoint().trim_end_matches("/v1").to_owned() + "/mcp";
    let (archive, sha) = package(&endpoint);
    let http = Client::builder()
        .no_proxy()
        .default_headers(support::compatibility_headers())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let mut commands = host.connect();
    let receipt=commands.runtime("begin_plugin_install",json!({"key":"f05-install","packages":[{
        "name":"managed-fixture","version":"1.0.0","sha256":sha,"size_bytes":archive.len(),"format":"tar_gzip","origin":{"type":"upload"},"expected_active_sha256":null
    }]}));
    assert_eq!(receipt["phase"], "accepted");
    let uploaded = http
        .post(format!(
            "{}/plugins/operations/f05-install/package",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .body(archive)
        .send()
        .unwrap();
    assert!(
        uploaded.status().is_success(),
        "{}",
        uploaded.text().unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let operation = commands.runtime("get_plugin_operation", json!({"key":"f05-install"}));
        if operation["phase"] == "succeeded" {
            break;
        }
        assert_ne!(operation["phase"], "failed", "{operation}");
        assert!(Instant::now() < deadline, "install did not settle");
        std::thread::sleep(Duration::from_millis(20));
    }
    let initial = configuration(&mut commands);
    let missing = &initial["servers"][0];
    assert_eq!(
        missing["source"],
        json!({"type":"plugin","name":"managed-fixture"})
    );
    assert_eq!(missing["managed"]["configuration_ready"], false);
    assert_eq!(
        missing["managed"]["parameter_bindings"]["Authorization"],
        json!(["token"])
    );
    assert_eq!(missing["managed"]["revision"], "absent");
    assert!(headers.lock().unwrap().is_empty());
    let plugin = commands.runtime("list_plugins", json!({}))["plugins"][0].clone();
    commands.runtime("mutate_plugin_settings",json!({"name":"managed-fixture","expected_active_sha256":sha,"expected_revision":plugin["settings_revision"],"mutation":{"type":"set_parameter","name":"token","value":"fake-parameter"}}));
    let before = configuration(&mut commands);
    let target = json!({"source":{"type":"plugin","name":"managed-fixture"},"server_key":"remote"});
    let server = &before["servers"][0];
    let changed=commands.runtime("mutate_mcp_configuration",json!({"expected_revision":server["managed"]["revision"],"mutation":{"type":"patch_managed","payload":{
        "target":target,"expected_definition_digest":server["managed"]["definition_digest"],"headers":{"x-custom":{"mode":"replace","value":"fake-custom"}}
    }}}))["snapshot"].clone();
    assert!(!user.join("mcp.json").exists());
    let overrides = user.join("plugins/managed-fixture/config/mcp-overrides.json");
    let saved = fs::read(&overrides).unwrap();
    let content: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(
        content["servers"]["remote"]["headers"]["x-custom"],
        "fake-custom"
    );
    assert!(
        !String::from_utf8(saved.clone())
            .unwrap()
            .contains("fake-parameter")
    );
    assert!(!changed.to_string().contains("fake-custom"));
    assert!(!changed.to_string().contains("fake-parameter"));
    assert_eq!(changed["servers"][0]["needs_refresh"], true);
    let mut refreshed = changed["servers"][0].clone();
    let tested=commands.runtime("test_mcp_server",json!({"test_id":"f05-test","managed":{
        "target":target,"expected_revision":refreshed["managed"]["revision"],"expected_definition_digest":refreshed["managed"]["definition_digest"],"headers":{"x-custom":{"mode":"replace","value":"temporary-draft"}}
    }}));
    assert_eq!(tested["outcome"], "success");
    assert_eq!(fs::read(&overrides).unwrap(), saved);
    assert_eq!(configuration(&mut commands)["servers"][0]["tool_count"], 0);
    let refresh=commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":refreshed["managed"]["revision"],"expected_definition_digest":refreshed["managed"]["definition_digest"]}));
    assert_eq!(refresh["outcome"], "success");
    refreshed = configuration(&mut commands)["servers"][0].clone();
    assert_eq!(refreshed["runtime_state"], "connected_without_tools");
    assert_eq!(refreshed["needs_refresh"], false);
    assert!(
        headers
            .lock()
            .unwrap()
            .iter()
            .any(
                |(token, custom)| token.as_deref() == Some("Bearer fake-parameter")
                    && custom.as_deref() == Some("temporary-draft")
            )
    );
    assert!(
        headers
            .lock()
            .unwrap()
            .iter()
            .any(
                |(token, custom)| token.as_deref() == Some("Bearer fake-parameter")
                    && custom.as_deref() == Some("fake-custom")
            )
    );
    // 同名用户配置按优先级接管；从被覆盖插件入口刷新也不能绕过选择。
    let current = configuration(&mut commands);
    let imported=commands.runtime("mutate_mcp_configuration",json!({"expected_revision":current["revision"],"mutation":{"type":"import","payload":{
        "document":json!({"mcpServers":{"remote":{"url":endpoint,"headers":{"x-custom":"user-priority"}}}}).to_string(),"replace_server_keys":[]
    }}}))["snapshot"].clone();
    assert!(imported["diagnostics"].as_array().unwrap().is_empty());
    let plugin_entry = imported["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["source"]["type"] == "plugin")
        .unwrap();
    assert_eq!(plugin_entry["shadowed_by"], json!({"type":"user"}));
    let refresh=commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":plugin_entry["managed"]["revision"],"expected_definition_digest":plugin_entry["managed"]["definition_digest"]}));
    assert_eq!(refresh["outcome"], "success");
    let current = configuration(&mut commands);
    let entries = current["servers"].as_array().unwrap();
    let active = entries
        .iter()
        .find(|entry| entry["source"]["type"] == "user")
        .unwrap();
    assert_eq!(active["runtime_state"], "connected_without_tools");
    let inactive = entries
        .iter()
        .find(|entry| entry["source"]["type"] == "plugin")
        .unwrap();
    assert_eq!(inactive["runtime_state"], "unavailable");
    assert_eq!(inactive["needs_refresh"], false);
    assert!(
        headers
            .lock()
            .unwrap()
            .iter()
            .any(|(_, custom)| custom.as_deref() == Some("user-priority"))
    );
    let disabled=commands.runtime("mutate_mcp_configuration",json!({"expected_revision":current["revision"],"mutation":{"type":"set_enabled","payload":{"server_key":"remote","enabled":false}}}))["snapshot"].clone();
    let plugin_entry = disabled["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["source"]["type"] == "plugin")
        .unwrap();
    assert!(plugin_entry.get("shadowed_by").is_none());
    let refresh=commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":plugin_entry["managed"]["revision"],"expected_definition_digest":plugin_entry["managed"]["definition_digest"]}));
    assert_eq!(refresh["outcome"], "success");
    let current = configuration(&mut commands);
    let plugin_entry = current["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["source"]["type"] == "plugin")
        .unwrap();
    assert_eq!(plugin_entry["runtime_state"], "connected_without_tools");
    commands.runtime("mutate_mcp_configuration",json!({"expected_revision":current["revision"],"mutation":{"type":"remove","payload":{"server_key":"remote"}}}));
    assert_eq!(fs::read(&overrides).unwrap(), saved);
    let blocked = http
        .put(format!(
            "{}/plugins/managed-fixture/config-files/mcp-overrides.json",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .header("x-ez-plugin-sha256", &sha)
        .header("x-ez-expected-revision", "absent")
        .body("{}")
        .send()
        .unwrap();
    assert!(!blocked.status().is_success());
    assert_eq!(fs::read(&overrides).unwrap(), saved);
    let listed: Value = http
        .get(format!(
            "{}/plugins/managed-fixture/config-files?sha256={sha}",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(listed["files"], json!([]));
    // 同一正式安装入口升级包：参数仍属原 plugins 行，直接覆盖仍在原 config 文件。
    let (archive, upgraded_sha) = package_version(&endpoint, "2.0.0");
    commands.runtime("begin_plugin_install",json!({"key":"f05-upgrade","packages":[{"name":"managed-fixture","version":"2.0.0","sha256":upgraded_sha,"size_bytes":archive.len(),"format":"tar_gzip","origin":{"type":"upload"},"expected_active_sha256":sha}]}));
    let uploaded = http
        .post(format!(
            "{}/plugins/operations/f05-upgrade/package",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .body(archive)
        .send()
        .unwrap();
    assert!(uploaded.status().is_success());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let operation = commands.runtime("get_plugin_operation", json!({"key":"f05-upgrade"}));
        if operation["phase"] == "succeeded" {
            break;
        }
        assert_ne!(operation["phase"], "failed", "{operation}");
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fs::read(&overrides).unwrap(), saved);
    let upgraded = configuration(&mut commands)["servers"][0].clone();
    assert_ne!(
        upgraded["managed"]["definition_digest"],
        refreshed["managed"]["definition_digest"]
    );
    let stale=http.post(format!("{}/commands",host.base_url())).bearer_auth(host.access_token()).json(&json!({"request_id":"f05-stale-package","command":{"scope":"runtime","payload":{"type":"refresh_mcp_server","payload":{"target":target,"expected_revision":refreshed["managed"]["revision"],"expected_definition_digest":refreshed["managed"]["definition_digest"]}}}})).send().unwrap();
    assert!(
        !stale.status().is_success(),
        "old package identity cannot refresh the replacement"
    );
    let refresh=commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":upgraded["managed"]["revision"],"expected_definition_digest":upgraded["managed"]["definition_digest"]}));
    assert_eq!(refresh["outcome"], "success");
    let sha = upgraded_sha;
    let before_disable = commands.runtime("list_plugins", json!({}))["plugins"][0].clone();
    commands.runtime("mutate_plugin_settings",json!({"name":"managed-fixture","expected_active_sha256":sha,"expected_revision":before_disable["settings_revision"],"mutation":{"type":"set_enabled","enabled":false}}));
    let disabled = configuration(&mut commands)["servers"][0].clone();
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["runtime_state"], "disabled");
    assert_eq!(disabled["needs_refresh"], false);
    commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":disabled["managed"]["revision"],"expected_definition_digest":disabled["managed"]["definition_digest"]}));
    assert_eq!(
        configuration(&mut commands)["servers"][0]["runtime_state"],
        "disabled"
    );
    audit(&database, 1, 2);
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let parameters: String = db
        .query_row(
            "SELECT parameters_json FROM plugins WHERE name='managed-fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&parameters).unwrap(),
        json!({"token":"fake-parameter"})
    );
    drop(db);
    commands.runtime("shutdown_runtime", json!({}));
    drop(commands);
    assert!(host.wait().status.success());
    let restarted = HostProcess::start(home.path());
    let mut commands = restarted.connect();
    let restored = configuration(&mut commands)["servers"][0].clone();
    assert_eq!(restored["enabled"], false);
    assert_eq!(restored["managed"]["headers"]["x-custom"], true);
    assert_eq!(fs::read(&overrides).unwrap(), saved);
    audit(&database, 1, 2);
    commands.runtime("shutdown_runtime", json!({}));
    drop(commands);
    assert!(restarted.wait().status.success());
    eprintln!(
        "F05 managed MCP isolated SQLite: plugins=1 plugin_operations=2 sessions=1(default controller) inputs=0 runs=0; parameter and override file authorities verified; temporary Home removed on exit"
    );
}

fn configuration(commands: &mut support::Client) -> Value {
    commands.runtime("get_mcp_configuration", json!({}))["snapshot"].clone()
}

fn audit(database: &Path, plugins: i64, operations: i64) {
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for (table, expected) in [
        ("plugins", plugins),
        ("plugin_operations", operations),
        ("sessions", 1),
        ("inputs", 0),
        ("runs", 0),
    ] {
        let actual: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(actual, expected, "table {table}");
    }
}

async fn mcp(
    State(seen): State<ObservedHeaders>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    let get = |key: &str| {
        headers
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    seen.lock()
        .unwrap()
        .push((get("authorization"), get("x-custom")));
    let result = match request["method"].as_str() {
        Some("server/discover") => {
            json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
        }
        Some("tools/list") => {
            json!({"resultType":"complete","tools":[],"ttlMs":0,"cacheScope":"private"})
        }
        other => panic!("unexpected method {other:?}"),
    };
    Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
}

fn package(endpoint: &str) -> (Vec<u8>, String) {
    package_version(endpoint, "1.0.0")
}

fn package_version(endpoint: &str, version: &str) -> (Vec<u8>, String) {
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    let manifest=json!({"format_version":1,"name":"managed-fixture","display_name":"Managed fixture","description":"Offline MCP","version":version,"platforms":[{"os":os,"arch":arch}],"min_host_version":"0.32.1","skills":[],"mcp":["remote"],"configuration":[{"name":"token","label":"Token","required":true,"secret":true}],"external_software":[]}).to_string();
    let descriptor=json!({"mcpServers":{"remote":{"url":endpoint,"headers":{"Authorization":"Bearer ${config.token}","x-custom":"package-default"}}}}).to_string();
    let gzip = GzEncoder::new(Vec::new(), Compression::fast());
    let mut tar = tar::Builder::new(gzip);
    for (name, value) in [
        ("plugin.json", manifest),
        ("mcp/remote/server.json", descriptor),
    ] {
        let bytes = value.as_bytes();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, name, Cursor::new(bytes))
            .unwrap();
    }
    let archive = tar.into_inner().unwrap().finish().unwrap();
    let sha = format!("{:x}", Sha256::digest(&archive));
    (archive, sha)
}
