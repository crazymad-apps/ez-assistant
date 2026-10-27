//! 新建隔离 Home，正式 Host 通过普通工具读取离线同源手册并可靠恢复。
#[path = "v0_32_1_f06_guide/joint.rs"]
mod joint;
mod support;

use assistant_runtime::{BUILTIN_GUIDE_REFERENCES, BUILTIN_GUIDE_SKILL_NAME};
use axum::{Json, Router, extract::State, routing::post};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
use support::{FakeProvider, HostProcess};

#[derive(Default)]
struct Fixture {
    requests: Mutex<Vec<Value>>,
}

fn isolated() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("f06-guide-")
        .tempdir_in(fs::canonicalize(root).unwrap())
        .unwrap()
}

// 验证复制后的 Host，不依赖源码 cwd；该变量仅属于本测试，不进入产品配置。
fn start_host(home: &Path) -> HostProcess {
    match std::env::var_os("EZ_ASSISTANT_F06_PORTABLE_HOST") {
        Some(executable) => HostProcess::start_with_executable(home, Path::new(&executable)),
        None => HostProcess::start(home),
    }
}

#[test]
fn formal_plan_reads_topics_and_preserves_results_across_override_disable_and_restart() {
    let home = isolated();
    let state = Arc::new(Fixture::default());
    let provider = FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(model))
            .with_state(state.clone()),
    );
    support::write_config(home.path(), provider.endpoint(), "fake-guide-model-key");
    let host = start_host(home.path());
    let mut client = host.connect();
    let provider_id =
        client.runtime("list_providers", json!({}))[0]["provider_instance_id"].clone();
    let mut parameters = support::model_parameters(false, false, "unsupported");
    parameters["context_window_tokens"]["value"] = json!(65536);
    client.runtime("save_model_fixed_config", json!({"selection":{"provider_instance_id":provider_id,"model_id":"offline-model"},"parameters":parameters}));
    let list = client.runtime("list_skills", json!({"scope":{"type":"management"}}));
    let guide = list["snapshot"]["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == BUILTIN_GUIDE_SKILL_NAME)
        .unwrap();
    assert_eq!(guide["source"], "builtin");
    let detail = client.runtime("get_skill_detail", json!({"name":BUILTIN_GUIDE_SKILL_NAME}));
    for reference in BUILTIN_GUIDE_REFERENCES {
        assert!(
            detail["detail"]["body"]
                .as_str()
                .unwrap()
                .contains(reference)
        );
    }
    let session = client.runtime(
        "create_session",
        json!({"title":"F06 guide fixture","model_selection":null}),
    )["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let submit = client.submit_started_input(json!({"session_id":session,"message":"F06_READ_GUIDE","variant":"plan","idempotency_key":"guide"}));
    let run = client.wait_for_status(
        "read offline guide",
        &session,
        submit["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );
    assert_eq!(run["text"], "GUIDE_COMPLETE");
    let view = client.runtime("get_session_view", json!({"session_id":session}));
    let active = view["snapshot"]["value"]["active_skills"]
        .as_array()
        .unwrap();
    assert_eq!(
        active.len(),
        1,
        "references are normal ToolMessages, not Skill activations"
    );
    assert_eq!(active[0]["tag"]["name"], BUILTIN_GUIDE_SKILL_NAME);

    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 7);
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains(BUILTIN_GUIDE_SKILL_NAME));
    assert!(
        !system.contains("# EZ Assistant 操作指引"),
        "ordinary catalog never loads full instructions"
    );
    assert!(
        requests[1]
            .to_string()
            .contains("builtin_skill: ez-assistant-guide")
    );
    for index in 0..4 {
        let tool = requests[index + 2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .unwrap();
        let content: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
        assert_eq!(content["reference"], BUILTIN_GUIDE_REFERENCES[index]);
        let source = [
            include_str!("../../../docs/site/manual/runtime-environment.md"),
            include_str!("../../../docs/site/manual/general-assistant.md"),
            include_str!("../../../docs/site/manual/mcp-configuration.md"),
            include_str!("../../../docs/site/manual/plugin-development.md"),
        ][index];
        assert_eq!(content["content"], source.split_once("\n---\n").unwrap().1);
    }
    assert!(
        requests[6]
            .to_string()
            .contains("not available in the current guide")
    );
    drop(requests);

    let user = home.path().join("users/_personal");
    let custom = user.join("skills/ez-assistant-guide");
    fs::create_dir_all(&custom).unwrap();
    fs::write(custom.join("SKILL.md"), "---\nname: ez-assistant-guide\ndescription: Custom guide fixture\n---\nCustom replacement only.\n").unwrap();
    assert_ne!(
        client.runtime("get_skill_detail", json!({"name":BUILTIN_GUIDE_SKILL_NAME}))["detail"]["source"],
        "builtin"
    );
    for (key, disable) in [("override", false), ("disabled", true)] {
        if disable {
            client.runtime(
                "set_skill_enabled",
                json!({"name":BUILTIN_GUIDE_SKILL_NAME,"enabled":false}),
            );
        }
        let submit = client.submit_started_input(
            json!({"session_id":session,"message":key,"variant":"plan","idempotency_key":key}),
        );
        let run = client.wait_for_status(
            "guide unavailable",
            &session,
            submit["run"]["run_id"].as_str().unwrap(),
            &["completed"],
        );
        assert_eq!(run["text"], "GUIDE_UNAVAILABLE");
    }
    let body = user
        .join("data/sessions")
        .join(&session)
        .join("conversation.1.jsonl");
    let frozen = fs::read(&body).unwrap();
    assert!(String::from_utf8_lossy(&frozen).contains("read_skill_reference"));
    let database = user.join("data/runtime.sqlite3");
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for (table, count) in [
        ("sessions", 2),
        ("inputs", 3),
        ("runs", 0),
        ("skill_activations", 0),
        ("model_request_records", 9),
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            count,
            "{table}"
        );
    }
    drop(db);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    let before = state.requests.lock().unwrap().len();
    let host = start_host(home.path());
    let mut client = host.connect();
    let view = client.runtime("get_session_view", json!({"session_id":session}));
    assert_eq!(
        view["snapshot"]["value"]["active_skills"][0]["tag"]["name"],
        BUILTIN_GUIDE_SKILL_NAME
    );
    assert_eq!(
        fs::read(&body).unwrap(),
        frozen,
        "reading history preserves exact results despite current override/disable"
    );
    assert_eq!(
        state.requests.lock().unwrap().len(),
        before,
        "restoration does not rerun model or tools"
    );
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    eprintln!(
        "F06 isolated local SQLite {}: sessions=2 inputs=3 model_request_records=9 runs=0 skill_activations=0; exact files and API verified, disposable fixture removed",
        database.display()
    );
}

async fn model(
    State(state): State<Arc<Fixture>>,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let tools = request["tools"].as_array();
    let has = |name| tools.is_some_and(|tools| tools.iter().any(|t| t["function"]["name"] == name));
    let mut step = 0;
    if has("load_skill") {
        let mut requests = state.requests.lock().unwrap();
        step = requests.len();
        assert!(step < 12);
        requests.push(request.clone());
    }
    let call = if !has("read_skill_reference") {
        None
    } else {
        match step {
            0 => Some(("load_skill", json!({"name":BUILTIN_GUIDE_SKILL_NAME}))),
            1..=4 => Some((
                "read_skill_reference",
                json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":BUILTIN_GUIDE_REFERENCES[step-1]}),
            )),
            5 => Some((
                "read_skill_reference",
                json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":"../outside.md"}),
            )),
            _ => None,
        }
    };
    wire(
        step,
        call,
        if has("read_skill_reference") {
            "GUIDE_COMPLETE"
        } else {
            "GUIDE_UNAVAILABLE"
        },
    )
}

fn wire(
    step: usize,
    call: Option<(&str, Value)>,
    text: &str,
) -> ([(&'static str, &'static str); 1], String) {
    let (delta, finish) = match call {
        Some((name, args)) => (
            json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("f06-tool-{step}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
            "tool_calls",
        ),
        None => (json!({"role":"assistant","content":text}), "stop"),
    };
    let frames = [
        json!({"id":format!("f06-{step}"),"model":"offline-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"id":format!("f06-{step}"),"model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}}),
    ];
    (
        [("content-type", "text/event-stream")],
        frames
            .iter()
            .map(|f| format!("data: {f}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n",
    )
}
