use super::*;
use axum::http::HeaderMap;
use std::sync::atomic::{AtomicUsize, Ordering};

fn prepare(client: &mut support::Client) -> String {
    let provider = client.runtime("list_providers", json!({}))[0]["provider_instance_id"].clone();
    let mut parameters = support::model_parameters(false, false, "unsupported");
    parameters["context_window_tokens"]["value"] = json!(65536);
    client.runtime("save_model_fixed_config", json!({"selection":{"provider_instance_id":provider,"model_id":"offline-model"},"parameters":parameters}));
    let session = client.runtime(
        "create_session",
        json!({"title":"F06 joint fixture","model_selection":null}),
    )["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"auto"}),
    );
    session
}

fn has_tool(request: &Value, name: &str) -> bool {
    request["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|t| t["function"]["name"] == name))
}

#[test]
fn child_reads_guide_without_receiving_parent_mcp_management() {
    let home = isolated();
    let state = Arc::new(Fixture::default());
    let provider = FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(child_model))
            .with_state(state.clone()),
    );
    support::write_config(home.path(), provider.endpoint(), "fake-child-model-key");
    let host = start_host(home.path());
    let mut client = host.connect();
    let session = prepare(&mut client);
    let submitted = client.submit_started_input(json!({"session_id":session,"message":"F06 delegate a guide topic read","variant":"build","idempotency_key":"child-guide"}));
    let run_id = submitted["run"]["run_id"].as_str().unwrap();
    let run = client.wait_for_status("child guide", &session, run_id, &["completed"]);
    assert_eq!(run["text"], "PARENT_COMPLETE");
    let tasks = client.runtime(
        "list_child_tasks",
        json!({"session_id":session,"parent_run_id":run_id}),
    );
    let task = &tasks["tasks"][0];
    assert_eq!(task["status"], "completed");
    assert_eq!(task["final_text"], "CHILD_GUIDE_COMPLETE");
    let child = client.runtime(
        "get_child_task_view",
        json!({"session_id":session,"child_task_id":task["child_task_id"]}),
    );
    assert!(child.to_string().contains("read_skill_reference"));
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    let child_requests: Vec<_> = requests
        .iter()
        .filter(|r| !has_tool(r, "delegate_task"))
        .collect();
    assert_eq!(child_requests.len(), 2);
    for request in child_requests {
        assert!(has_tool(request, "read_skill_reference"));
        for name in [
            "get_mcp_configuration",
            "configure_mcp",
            "refresh_mcp",
            "delegate_task",
        ] {
            assert!(!has_tool(request, name));
        }
    }
    drop(requests);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

async fn child_model(
    State(state): State<Arc<Fixture>>,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    if !has_tool(&request, "load_skill") {
        return wire(90, None, "fixture title");
    }
    let step = {
        let mut requests = state.requests.lock().unwrap();
        let step = requests.len();
        assert!(step < 6);
        requests.push(request.clone());
        step
    };
    let returned = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "tool");
    if has_tool(&request, "delegate_task") {
        let call = (!returned).then(|| ("delegate_task",json!({"title":"Read guide","task":"Read the MCP topic via read_skill_reference and return CHILD_GUIDE_COMPLETE.","context":null,"expected_output":"CHILD_GUIDE_COMPLETE"})));
        wire(step, call, "PARENT_COMPLETE")
    } else {
        let call = (!returned).then(|| {
            (
                "read_skill_reference",
                json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":"references/mcp.md"}),
            )
        });
        wire(step, call, "CHILD_GUIDE_COMPLETE")
    }
}

struct Operations {
    requests: Mutex<Vec<Value>>,
    searches: AtomicUsize,
    endpoint: Mutex<String>,
    output: std::path::PathBuf,
}

const GENERATED: &str = "---\nname: guide-generated\ndescription: Skill created after reading the embedded guide\n---\nUse this verified method.\n";

#[test]
fn guide_drives_skill_creation_and_mcp_save_enable_refresh_then_next_run_call() {
    let home = isolated();
    let state = Arc::new(Operations {
        requests: Mutex::new(Vec::new()),
        searches: AtomicUsize::new(0),
        endpoint: Mutex::new(String::new()),
        output: home
            .path()
            .join("users/_personal/skills/guide-generated/SKILL.md"),
    });
    let provider = FakeProvider::with_router(
        Router::new()
            .route("/v1/chat/completions", post(operations_model))
            .route("/v1/tools/search", post(search))
            .with_state(state.clone()),
    );
    *state.endpoint.lock().unwrap() = provider.endpoint().to_owned();
    support::write_config(
        home.path(),
        provider.endpoint(),
        "fake-operations-model-key",
    );
    let host = start_host(home.path());
    let mut client = host.connect();
    let session = prepare(&mut client);
    fs::create_dir_all(state.output.parent().unwrap()).unwrap();
    let started=client.submit_started_input(json!({"session_id":session,"message":"Read the Skill and MCP guide topics. Create guide-generated, save isolated MCP values while disabled, then explicitly enable and refresh builtin search; do not search yet.","variant":"build","idempotency_key":"configure-from-guide"}));
    let run = client.wait_for_status(
        "guide operations",
        &session,
        started["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );
    assert_eq!(run["text"], "CONFIGURED_WITH_GUIDE");
    assert_eq!(
        state.searches.load(Ordering::SeqCst),
        0,
        "read/save/enable/refresh never perform business calls"
    );
    assert_eq!(fs::read_to_string(&state.output).unwrap(), GENERATED);
    let detail = client.runtime("get_skill_detail", json!({"name":"guide-generated"}));
    assert_eq!(detail["detail"]["body"], "Use this verified method.\n");
    let started=client.submit_started_input(json!({"session_id":session,"message":"Load guide-generated to verify the new Skill, then search through the explicitly enabled service.","variant":"build","idempotency_key":"search-next-run"}));
    let run = client.wait_for_status(
        "guide next Run",
        &session,
        started["run"]["run_id"].as_str().unwrap(),
        &["completed"],
    );
    assert_eq!(run["text"], "GUIDE_OPERATIONS_COMPLETE");
    assert_eq!(state.searches.load(Ordering::SeqCst), 1);
    let requests = state.requests.lock().unwrap();
    assert_eq!(requests.len(), 15);
    assert!(!has_tool(&requests[0], "call_mcp_tool"));
    assert!(has_tool(&requests[11], "call_mcp_tool"));
    assert!(
        requests[3]
            .to_string()
            .contains("builtin_skill: skill-creator")
    );
    assert!(
        requests[14]
            .to_string()
            .contains("https://example.org/f06-source")
    );
    assert!(
        requests[12]
            .to_string()
            .contains("Use this verified method.")
    );
    for index in [5, 7, 9] {
        let result = requests[index]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .unwrap();
        assert!(!result.to_string().contains("fake-f06-search-key"));
    }
    drop(requests);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
}

async fn operations_model(
    State(state): State<Arc<Operations>>,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    if !has_tool(&request, "load_skill") {
        return wire(90, None, "fixture title");
    }
    let step = {
        let mut r = state.requests.lock().unwrap();
        let step = r.len();
        assert!(step < 16);
        r.push(request.clone());
        step
    };
    let target = json!({"source":{"type":"builtin"},"server_key":"deepseek-search"});
    let current = || {
        let result = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "tool")
            .unwrap();
        let result: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
        let server = &result["servers"][0];
        if step == 7 {
            assert_eq!(
                server["enabled"], false,
                "saving Key preserves disabled intent"
            );
        }
        json!({"target":target,"expected_revision":server["managed"]["revision"],"expected_definition_digest":server["managed"]["definition_digest"]})
    };
    let call = match step {
        0 => Some((
            "read_skill_reference",
            json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":"references/skills.md"}),
        )),
        1 => Some(("load_skill", json!({"name":"skill-creator"}))),
        2 => Some((
            "write_file",
            json!({"path":state.output,"content":GENERATED}),
        )),
        3 => Some((
            "read_skill_reference",
            json!({"name":BUILTIN_GUIDE_SKILL_NAME,"reference":"references/mcp.md"}),
        )),
        4 | 6 | 8 => Some(("get_mcp_configuration", json!({"target":target}))),
        5 => Some((
            "configure_mcp",
            json!({"expected":current(),"change":{"type":"patch","enabled":false,"environment":{"DEEPSEEK_API_KEY":{"mode":"replace","value":"fake-f06-search-key"},"DEEPSEEK_BASE_URL":{"mode":"replace","value":*state.endpoint.lock().unwrap()}},"headers":{}}}),
        )),
        7 => Some((
            "configure_mcp",
            json!({"expected":current(),"change":{"type":"patch","enabled":true,"environment":{},"headers":{}}}),
        )),
        9 => Some(("refresh_mcp", current())),
        11 => Some(("load_skill", json!({"name":"guide-generated"}))),
        12 => Some((
            "discover_mcp_tools",
            json!({"server":"deepseek-search","detail":"full"}),
        )),
        13 => Some((
            "call_mcp_tool",
            json!({"server":"deepseek-search","tool":"deepseek_search","arguments":{"query":"F06 isolated search"}}),
        )),
        _ => None,
    };
    wire(
        step,
        call,
        if step == 10 {
            "CONFIGURED_WITH_GUIDE"
        } else {
            "GUIDE_OPERATIONS_COMPLETE"
        },
    )
}

async fn search(
    State(state): State<Arc<Operations>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fake-f06-search-key");
    assert_eq!(request["query"], "F06 isolated search");
    state.searches.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"results":[{"rerank_score":2,"title":"F06 source","url":"https://example.org/f06-source","snippet":"fixture","summary":"fixture","content":"","site_name":null,"site_icon":null,"date":null,"authority":1}]}),
    )
}
