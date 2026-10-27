//! 新建一次性隔离 Home；正式二进制的内置资源和标准 MCP stdio 验证。
#[path = "v0_32_1_f05_builtin/acceptance.rs"]
mod acceptance;
mod support;

use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use rmcp::{ServiceExt, model::CallToolRequestParams};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{FakeProvider, HostProcess};
use tokio::process::{Child, Command};

fn isolated(prefix: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .unwrap()
}

fn stdio(executable: &Path, home: &Path, key: &str, base: &str) -> Child {
    Command::new(executable)
        .current_dir(home.parent().expect("isolated fixture directory"))
        .args(["mcp", "deepseek-search", "--stdio"])
        .env_clear()
        .env("EZ_ASSISTANT_RUNTIME_HOME", home)
        .env("HOME", home)
        .env("DEEPSEEK_API_KEY", key)
        .env("DEEPSEEK_BASE_URL", base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

#[tokio::test]
async fn bundled_stdio_works_away_from_sources_without_a_runtime_home() {
    let fixture = isolated("f05-m2-stdio-");
    let executable = fixture
        .path()
        .join(if cfg!(windows) { "tool.exe" } else { "tool" });
    fs::copy(env!("CARGO_BIN_EXE_ez-assistant-runtime"), &executable).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let remote = FakeProvider::with_router(
        Router::new()
            .route("/v1/tools/search", post(search))
            .with_state(calls.clone()),
    );
    for key in ["", "fake-key"] {
        let home = fixture.path().join(if key.is_empty() {
            "absent-key-home"
        } else {
            "search-home"
        });
        let mut child = stdio(&executable, &home, key, remote.endpoint());
        let service =
            ().serve((child.stdout.take().unwrap(), child.stdin.take().unwrap()))
                .await
                .unwrap();
        let tools = service.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "deepseek_search");
        let request = CallToolRequestParams::new("deepseek_search").with_arguments(
            json!({"query":"site:example.org 中文"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let response = service.call_tool(request).await.unwrap();
        if key.is_empty() {
            assert_eq!(response.is_error, Some(true));
            assert!(
                serde_json::to_string(&response)
                    .unwrap()
                    .contains("DEEPSEEK_API_KEY")
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        } else {
            assert_ne!(response.is_error, Some(true));
            assert_eq!(
                response.structured_content.unwrap()["results"][0]["url"],
                "https://example.org/original?q=1"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let large = service
                .call_tool(
                    CallToolRequestParams::new("deepseek_search")
                        .with_arguments(json!({"query":"large"}).as_object().unwrap().clone()),
                )
                .await
                .unwrap();
            assert_eq!(large.is_error, Some(true));
            assert!(
                serde_json::to_string(&large)
                    .unwrap()
                    .contains("减少 count")
            );
        }
        service.cancel().await.unwrap();
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !home.exists(),
            "stdio mode must not create Home, DB, lock or discovery files"
        );
    }
}

async fn search(
    State(calls): State<Arc<AtomicUsize>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fake-key");
    assert!(
        ["site:example.org 中文", "large"]
            .iter()
            .any(|query| request["query"] == *query)
    );
    calls.fetch_add(1, Ordering::SeqCst);
    let content = if request["query"] == "large" {
        "x".repeat(4 * 1024 * 1024 + 1024)
    } else {
        String::new()
    };
    Json(
        json!({"results":[{"rerank_score":2.5,"title":"Source","url":"https://example.org/original?q=1","snippet":"summary","summary":"summary","content":content,"site_name":null,"site_icon":null,"date":null,"authority":1}]}),
    )
}

#[tokio::test]
async fn stdio_rejects_fifth_call_and_eof_cancels_owned_requests() {
    let fixture = isolated("f05-m2-cancel-");
    let (sender, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let remote = FakeProvider::with_router(Router::new().route(
        "/v1/tools/search",
        post(move || {
            let sender = sender.clone();
            async move {
                sender.send(()).unwrap();
                axum::body::Body::from_stream(futures_util::stream::pending::<
                    Result<String, std::io::Error>,
                >())
            }
        }),
    ));
    let home = fixture.path().join("unused-home");
    let mut child = stdio(
        Path::new(env!("CARGO_BIN_EXE_ez-assistant-runtime")),
        &home,
        "fake-key",
        remote.endpoint(),
    );
    let service = Arc::new(
        ().serve((child.stdout.take().unwrap(), child.stdin.take().unwrap()))
            .await
            .unwrap(),
    );
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let service = service.clone();
        tasks.push(tokio::spawn(async move {
            service
                .call_tool(
                    CallToolRequestParams::new("deepseek_search")
                        .with_arguments(json!({"query":"hold"}).as_object().unwrap().clone()),
                )
                .await
        }));
    }
    for _ in 0..4 {
        tokio::time::timeout(Duration::from_secs(5), observed.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let busy = service
        .call_tool(
            CallToolRequestParams::new("deepseek_search")
                .with_arguments(json!({"query":"fifth"}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_eq!(busy.is_error, Some(true));
    assert!(serde_json::to_string(&busy).unwrap().contains("4 个请求"));
    assert!(observed.try_recv().is_err());
    service.cancellation_token().cancel();
    for task in tasks {
        let _response = task.await.unwrap();
    }
    let service = Arc::try_unwrap(service).ok().unwrap();
    service.waiting().await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(4), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!home.exists());
}

#[test]
fn formal_host_defaults_save_test_enable_and_restart_use_one_configuration() {
    let home = isolated("f05-m2-product-");
    let model = FakeProvider::start();
    support::write_config(home.path(), model.endpoint(), "fake-model-key");
    let executable = home.path().join(if cfg!(windows) {
        "bundled-host.exe"
    } else {
        "bundled-host"
    });
    fs::copy(env!("CARGO_BIN_EXE_ez-assistant-runtime"), &executable).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let remote = FakeProvider::with_router(
        Router::new()
            .route("/v1/tools/search", post(search))
            .with_state(calls.clone()),
    );
    // 无源库；目标为本机 Home/users/_personal/data/runtime.sqlite3，端口不适用。
    // 初始化仅默认控制会话；配置不产生 Run。后续两个显式 Skill 输入新增一会话、两条输入身份，终态 Run 仅留文件；名称禁用在原 skill_name_states 单行。
    let host = HostProcess::start_with_executable(home.path(), &executable);
    let user = home.path().join("users/_personal");
    let database = user.join("data/runtime.sqlite3");
    audit(&database, 0, 0);
    let mut commands = host.connect();
    let initial = configuration(&mut commands);
    assert_eq!(initial["source"], json!({"type":"builtin"}));
    assert_eq!(initial["display_name"], "DeepSeek 搜索");
    assert_eq!(initial["tool_timeout_ms"], 180_000);
    assert_eq!(initial["enabled"], false);
    assert_eq!(initial["runtime_state"], "disabled");
    assert_eq!(initial["tool_count"], 0);
    assert!(!user.join("mcp.json").exists());
    assert!(
        !user.join("mcp").exists(),
        "disabled builtin must not spawn stdio"
    );
    let preview=commands.runtime("preview_mcp_import",json!({"document":json!({"mcpServers":{"deepseek-search":{"command":"unrelated"}}}).to_string()}));
    assert_eq!(preview["entries"][0]["conflicts_with_existing"], false);
    let target = json!({"source":{"type":"builtin"},"server_key":"deepseek-search"});
    let saved = patch(
        &mut commands,
        &initial,
        json!({"environment":{
            "DEEPSEEK_API_KEY":{"mode":"replace","value":"fake-key"},
            "DEEPSEEK_BASE_URL":{"mode":"replace","value":remote.endpoint()}
        }}),
    );
    assert_eq!(
        saved["enabled"], false,
        "saving a Key must not enable the service"
    );
    assert_eq!(saved["runtime_state"], "disabled");
    assert!(!saved.to_string().contains("fake-key"));
    assert!(!user.join("mcp").exists());
    let file: Value = serde_json::from_slice(&fs::read(user.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        file["managedServers"]["builtin"]["deepseek-search"]["env"]["DEEPSEEK_API_KEY"],
        "fake-key"
    );
    let tested=commands.runtime("test_mcp_server",json!({"test_id":"builtin-handshake","managed":{
        "target":target,"expected_revision":saved["managed"]["revision"],"expected_definition_digest":saved["managed"]["definition_digest"]
    }}));
    assert_eq!(tested["outcome"], "success", "{tested}");
    assert_eq!(tested["tool_count"], 1);
    assert_eq!(configuration(&mut commands)["runtime_state"], "disabled");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "test must not search");
    let enabled = patch(&mut commands, &saved, json!({"enabled":true}));
    let refreshed=commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":enabled["managed"]["revision"],"expected_definition_digest":enabled["managed"]["definition_digest"]}));
    assert_eq!(refreshed["outcome"], "success", "{refreshed}");
    let connected = configuration(&mut commands);
    assert_eq!(connected["runtime_state"], "connected");
    assert_eq!(connected["tool_count"], 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "enabling only connects, never searches"
    );
    let removed = patch(
        &mut commands,
        &connected,
        json!({"environment":{"DEEPSEEK_API_KEY":{"mode":"remove"}}}),
    );
    let refresh=commands.runtime("refresh_mcp_server",json!({"target":target,"expected_revision":removed["managed"]["revision"],"expected_definition_digest":removed["managed"]["definition_digest"]}));
    assert_eq!(refresh["outcome"], "success");
    let connected = configuration(&mut commands);
    assert_eq!(
        connected["managed"]["environment"]["DEEPSEEK_API_KEY"],
        false
    );
    let listed = commands.runtime("list_skills", json!({}));
    let creator = listed["snapshot"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["name"] == "skill-creator")
        .unwrap();
    assert_eq!(creator["source"], "builtin");
    let detail = commands.runtime("get_skill_detail", json!({"name":"skill-creator"}));
    assert!(
        detail["detail"]["body"]
            .as_str()
            .unwrap()
            .contains("SKILL.md")
    );
    assert!(!user.join("skills/skill-creator").exists());
    let session = commands.runtime(
        "create_session",
        json!({"title":"Builtin activation","model_selection":null}),
    )["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    activate(&mut commands, &session, "skill-creator", "builtin");
    let frozen = commands.runtime("get_session_view", json!({"session_id":session}));
    assert_eq!(
        frozen["snapshot"]["value"]["active_skills"][0]["tag"]["name"],
        "skill-creator"
    );
    let custom = user.join("skills/skill-creator");
    fs::create_dir_all(&custom).unwrap();
    fs::write(custom.join("SKILL.md"),"---\nname: skill-creator\ndescription: Custom creator fixture\n---\nCustom replacement instructions.\n").unwrap();
    let custom = commands.runtime("get_skill_detail", json!({"name":"skill-creator"}));
    assert_eq!(custom["detail"]["skill"]["source"], "user_ez_assistant");
    assert_eq!(
        custom["detail"]["body"],
        "Custom replacement instructions.\n"
    );
    assert_eq!(
        commands.runtime("get_session_view", json!({"session_id":session}))["snapshot"]["value"]["active_skills"],
        frozen["snapshot"]["value"]["active_skills"]
    );

    let generated = user.join("skills/generated-fixture");
    fs::create_dir_all(&generated).unwrap();
    fs::write(generated.join("SKILL.md"),"---\nname: generated-fixture\ndescription: Generated output fixture\n---\nUse the generated instructions.\n").unwrap();
    let generated = commands.runtime("get_skill_detail", json!({"name":"generated-fixture"}));
    assert_eq!(
        generated["detail"]["body"],
        "Use the generated instructions.\n"
    );
    activate(&mut commands, &session, "generated-fixture", "generated");
    let disabled = commands.runtime(
        "set_skill_enabled",
        json!({"name":"skill-creator","enabled":false}),
    );
    assert!(
        disabled["snapshot"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "skill-creator" && skill["health"] == "disabled")
    );
    let active=commands.runtime("get_session_view",json!({"session_id":session}))["snapshot"]["value"]["active_skills"].clone();
    assert_eq!(active.as_array().unwrap().len(), 2);
    audit(&database, 1, 2);
    commands.runtime("shutdown_runtime", json!({}));
    drop(commands);
    assert!(host.wait().status.success());
    let host = HostProcess::start_with_executable(home.path(), &executable);
    let mut commands = host.connect();
    let restored = configuration(&mut commands);
    assert_eq!(
        commands.runtime("get_session_view", json!({"session_id":session}))["snapshot"]["value"]["active_skills"],
        active
    );

    assert_eq!(restored["enabled"], true);
    assert_eq!(
        restored["managed"]["revision"],
        connected["managed"]["revision"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let disabled = commands.runtime("list_skills", json!({}));
    assert!(
        disabled["snapshot"]["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "skill-creator" && skill["health"] == "disabled")
    );
    audit(&database, 1, 2);
    commands.runtime("shutdown_runtime", json!({}));
    drop(commands);
    assert!(host.wait().status.success());
    eprintln!(
        "F05 M2 local SQLite {}: sessions=2, inputs=2, runs=0, plugins=0, plugin_operations=0, skill_name_states=1, session_usage=2, model_request_records=2, skill_activations=0, mcp_input_selections=0; MCP override file and Skill enable state roundtrip verified; disposable fixture removed",
        database.display()
    );
}
fn configuration(commands: &mut support::Client) -> Value {
    commands.runtime("get_mcp_configuration", json!({}))["snapshot"]["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|server| {
            server["server_key"] == "deepseek-search"
                && server["source"] == json!({"type":"builtin"})
        })
        .unwrap()
        .clone()
}
fn patch(commands: &mut support::Client, server: &Value, fields: Value) -> Value {
    let mut payload = fields.as_object().unwrap().clone();
    payload.insert(
        "target".into(),
        json!({"source":{"type":"builtin"},"server_key":"deepseek-search"}),
    );
    payload.insert(
        "expected_definition_digest".into(),
        server["managed"]["definition_digest"].clone(),
    );
    commands.runtime("mutate_mcp_configuration",json!({"expected_revision":server["managed"]["revision"],"mutation":{"type":"patch_managed","payload":payload}}));
    configuration(commands)
}
fn audit(path: &Path, skill_states: i64, inputs: i64) {
    let database = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT COALESCE(SUM(request_count),0) FROM session_usage",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        inputs
    );
    if skill_states == 1 {
        assert_eq!(
            database
                .query_row("SELECT name, enabled FROM skill_name_states", [], |row| Ok(
                    (row.get::<_, String>(0)?, row.get::<_, i64>(1)?)
                ))
                .unwrap(),
            ("skill-creator".into(), 0)
        );
    }
    for (table, expected) in [
        ("sessions", 1 + i64::from(inputs > 0)),
        ("inputs", inputs),
        ("runs", 0),
        ("plugins", 0),
        ("plugin_operations", 0),
        ("skill_name_states", skill_states),
        ("session_usage", 1 + i64::from(inputs > 0)),
        ("model_request_records", inputs),
        ("skill_activations", 0),
        ("mcp_input_selections", 0),
    ] {
        assert_eq!(
            database
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            expected,
            "{table}"
        );
    }
}

fn activate(commands: &mut support::Client, session: &str, name: &str, key: &str) {
    eprintln!("activating isolated Skill {name}");
    // Fixture 的响应身份由 case 决定，第二轮必须使用不同 case，遵守真实历史唯一键。
    let message = if key == "generated" {
        "REPLACEMENT_CASE"
    } else {
        "SKILL_FORMAL_CASE"
    };
    let started=commands.submit_started_input(json!({"session_id":session,"message":message,"variant":"build","skill_name":name,"idempotency_key":key}));
    let run = started["run"]["run_id"].as_str().unwrap();
    assert_eq!(
        commands.wait_for_status("Skill activation", session, run, &["completed"])["status"],
        "completed"
    );
}
