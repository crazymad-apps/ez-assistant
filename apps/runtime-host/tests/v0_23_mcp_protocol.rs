#![cfg(unix)]

mod support;

#[path = "v0_23_mcp_protocol/mod.rs"]
mod invocation;

use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use tempfile::TempDir;

use support::{Client, FakeProvider, HostProcess, write_config};

#[test]
fn formal_host_manages_mcp_and_persists_refresh_without_creating_runs() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "offline-mcp-provider-key",
    );
    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py");
    let draft = json!({
        "server_key":"local_fixture", "display_name":"Local fixture",
        "description":"Two safe fixture tools", "enabled":true,
        "transport":{"type":"stdio","payload":{
            "command":{"mode":"replace","value":"python3"},
            "args":{"mode":"replace","value":["-u",fixture]},
            "environment":{"TOKEN":{"mode":"replace","value":"offline-mcp-secret"}}
        }}
    });

    // 测试使用真实 stdio Adapter，但不保存配置或修改活动目录。
    let tested = client.runtime(
        "test_mcp_server",
        json!({"test_id":"unsaved-test","server":draft}),
    );
    assert_eq!(tested["outcome"], "success");
    assert_eq!(tested["tool_count"], 2);
    let initial = configuration(&mut client);
    assert_eq!(initial["servers"], json!([]));
    assert_eq!(initial["needs_refresh"], false);

    let document = json!({"mcpServers":{"local_fixture":{
        "command":"python3", "args":["-u",fixture],
        "env":{"TOKEN":"offline-mcp-secret"}
    }}})
    .to_string();
    let preview = client.runtime("preview_mcp_import", json!({"document":document}));
    assert_eq!(preview["entries"][0]["conflicts_with_existing"], false);
    assert!(!preview.to_string().contains("offline-mcp-secret"));
    let imported = client.runtime(
        "mutate_mcp_configuration",
        json!({
            "expected_revision":initial["revision"],
            "mutation":{"type":"import","payload":{"document":document}}
        }),
    )["snapshot"]
        .clone();
    assert_eq!(imported["needs_refresh"], true);
    assert_eq!(imported["servers"][0]["tool_count"], 0);
    assert!(!imported.to_string().contains("offline-mcp-secret"));
    assert!(!imported.to_string().contains("mcp_stdio_server.py"));
    let session_id = client.runtime(
        "create_session",
        json!({
            "title":"MCP management", "model_key":"fixture"
        }),
    )["session"]["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();

    submit_refresh(&mut client, &session_id, "apply-import");
    let applied = wait_for_refreshes(&mut client, &session_id, 1);
    assert_eq!(
        applied["conversation"]["items"][0]["result"]["outcome"],
        "success"
    );
    let active = configuration(&mut client);
    assert_eq!(active["needs_refresh"], false);
    assert_eq!(active["servers"][0]["runtime_state"], "connected");
    assert_eq!(active["servers"][0]["tool_count"], 2);
    let options = client.runtime(
        "list_mcp_server_options",
        json!({
            "context":{"type":"session","payload":{"session_id":session_id}}, "variant":"build"
        }),
    );
    assert_eq!(options["servers"][0]["visible_tool_count"], 2);

    // 编辑脱敏快照时保持连接字段，测试仍能找到原参数；保存名称仍须刷新后生效。
    let kept_draft = json!({
        "server_key":"local_fixture", "display_name":"Renamed fixture",
        "description":"Two safe fixture tools", "enabled":true,
        "transport":{"type":"stdio","payload":{}}
    });
    let tested = client.runtime(
        "test_mcp_server",
        json!({"test_id":"kept-fields-test","server":kept_draft}),
    );
    assert_eq!(tested["outcome"], "success");
    assert_eq!(tested["tool_count"], 2);
    let edited = client.runtime(
        "mutate_mcp_configuration",
        json!({
            "expected_revision":active["revision"],
            "mutation":{"type":"upsert","payload":{"server":kept_draft}}
        }),
    )["snapshot"]
        .clone();
    assert_eq!(edited["servers"][0]["display_name"], "Renamed fixture");
    assert_eq!(edited["needs_refresh"], true);

    // 删除最后一项后仍可观察待刷新，不以空列表掩盖尚未关闭的连接。
    let removed = client.runtime(
        "mutate_mcp_configuration",
        json!({
            "expected_revision":edited["revision"],
            "mutation":{"type":"remove","payload":{"server_key":"local_fixture"}}
        }),
    )["snapshot"]
        .clone();
    assert_eq!(removed["servers"], json!([]));
    assert_eq!(removed["needs_refresh"], true);
    submit_refresh(&mut client, &session_id, "apply-delete");
    let settled = wait_for_refreshes(&mut client, &session_id, 2);
    assert_eq!(
        settled["conversation"]["items"][1]["result"]["servers"][0]["outcome"],
        "removed"
    );
    assert_eq!(configuration(&mut client)["needs_refresh"], false);
    let options = client.runtime(
        "list_mcp_server_options",
        json!({
            "context":{"type":"session","payload":{"session_id":session_id}}, "variant":"build"
        }),
    );
    assert_eq!(options["servers"], json!([]));
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());

    let restarted = HostProcess::start(runtime_home.path());
    let mut client = restarted.connect();
    let recovered = wait_for_refreshes(&mut client, &session_id, 2);
    assert_eq!(
        recovered["conversation"]["items"],
        settled["conversation"]["items"]
    );
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(restarted.wait().status.success());

    // 只读核验刚创建的隔离库；控制指令持久化但不创建 Run 或 Run body append。
    let connection = Connection::open_with_flags(
        runtime_home.path().join("data/runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open isolated database read-only");
    let counts: (i64, i64, i64) = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM inputs), (SELECT COUNT(*) FROM runs), (SELECT COUNT(*) FROM body_appends)",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).expect("exact isolated table counts");
    assert_eq!(counts, (2, 0, 0));
}

#[test]
#[ignore = "contacts public Microsoft Learn using an isolated Runtime Home; run explicitly"]
fn formal_host_tests_public_microsoft_learn_connection() {
    let provider = FakeProvider::start();
    let runtime_home = TempDir::new().expect("isolated Runtime Home");
    write_config(
        runtime_home.path(),
        provider.endpoint(),
        "offline-mcp-provider-key",
    );
    let host = HostProcess::start(runtime_home.path());
    let mut client = host.connect();
    // 与设置页相同的 Command，包含 Runtime 配置校验、Auto 握手、完整目录校验和关闭。
    // 仅提交临时草稿，不修改用户配置，不调用真实模型或远端业务工具。
    let result = client.runtime(
        "test_mcp_server",
        json!({
            "test_id":"public-learn-handshake",
            "server":{
                "server_key":"microsoft-learn", "display_name":"Microsoft Learn",
                "description":"Public Microsoft documentation", "enabled":true,
                "transport":{"type":"streamable_http","payload":{
                    "url":{"mode":"replace","value":"https://learn.microsoft.com/api/mcp"}
                }}
            }
        }),
    );
    let saved = configuration(&mut client);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    assert_eq!(result["outcome"], "success");
    let count = result["tool_count"].as_u64().expect("tool count");
    assert!(count > 0);
    assert_eq!(saved["servers"], json!([]), "test must not save the draft");
    eprintln!("Microsoft Learn connection test: success, {count} tools");
}

fn configuration(client: &mut Client) -> Value {
    client.runtime("get_mcp_configuration", json!({}))["snapshot"].clone()
}

fn submit_refresh(client: &mut Client, session_id: &str, key: &str) {
    let accepted = client.runtime(
        "submit_session_command",
        json!({
            "session_id":session_id, "idempotency_key":key,
            "command":{"type":"mcp_refresh","payload":{}}
        }),
    );
    assert_eq!(accepted["accepted"]["is_duplicate"], false);
}

fn wait_for_refreshes(client: &mut Client, session_id: &str, count: usize) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let view = client.runtime("get_session_view", json!({"session_id":session_id}))["snapshot"]
            ["value"]
            .clone();
        assert_eq!(view["runs"], json!([]), "refresh must not create a Run");
        let items = view["conversation"]["items"]
            .as_array()
            .expect("conversation items");
        if view["queue"]["items"] == json!([]) && items.len() == count {
            assert!(items.iter().all(|item| item["type"] == "control_result"));
            return view;
        }
        assert!(Instant::now() < deadline, "refresh did not settle: {view}");
        thread::sleep(Duration::from_millis(20));
    }
}
