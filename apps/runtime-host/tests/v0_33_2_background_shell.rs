//! 正式 Host + 脚本化 Provider 的后台 Shell 合同，运行目录全为新建隔离夹具。
#![cfg(unix)]

mod support;

use std::{
    io::Read as _,
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use support::{Client, FakeProvider, HostProcess, compatibility_headers, write_config};

fn submit(client: &mut Client, session: &str, message: &str, variant: &str) -> String {
    client.submit_started_input(json!({
        "session_id":session,"message":message,"variant":variant,"idempotency_key":message,
    }))["run"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn wait_approval(client: &mut Client, session: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        let approvals = client.runtime("list_pending_approvals", json!({"session_id":session}));
        if let Some(approval) = approvals["approvals"].as_array().unwrap().first() {
            return approval.clone();
        }
        assert!(
            Instant::now() < deadline,
            "background operation did not reach approval"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn detail(client: &mut Client, session: &str, name: &str) -> Value {
    let page = client.conversation(session);
    for item in page["items"].as_array().unwrap().iter().rev() {
        for segment in item["segments"].as_array().into_iter().flatten() {
            if segment["type"] != "tool_group" {
                continue;
            }
            for tool in segment["tools"].as_array().into_iter().flatten().rev() {
                if tool["tool_name"] == name {
                    return client.runtime(
                        "get_tool_detail",
                        json!({
                            "owner":{"type":"main_session","session_id":session},
                            "message_id":item["message_id"],"call_id":tool["call_id"],
                        }),
                    )["snapshot"]["value"]
                        .clone();
                }
            }
        }
    }
    panic!("missing detail for {name}");
}

fn result(detail: &Value) -> Value {
    serde_json::from_str(detail["result_json"].as_str().unwrap()).unwrap()
}

#[derive(Default)]
struct Script {
    responses: usize,
    issued_id: Option<String>,
    definitions: Vec<Value>,
    child_definitions: Option<Value>,
    child_result: Option<String>,
}

impl Script {
    fn answer(&mut self, request: &Value) -> String {
        let messages = request["messages"].as_array().unwrap();
        let last = messages.last().unwrap();
        let case = messages
            .iter()
            .rev()
            .filter(|message| message["role"] == "user")
            .map(|message| message["content"].to_string())
            .find(|content| content.contains("F01 fixture "))
            .unwrap_or_default();
        let tool = if last["role"] == "tool" {
            let id = last["tool_call_id"].as_str().unwrap();
            if id.ends_with("list_background_runs") && case.contains("manage") {
                let found: Value = serde_json::from_str(last["content"].as_str().unwrap()).unwrap();
                let id = found["runs"][0]["background_id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                self.issued_id = Some(id.clone());
                Some((
                    "read_background_output",
                    json!({"background_id":id,"limit_chars":2}),
                ))
            } else if id.ends_with("read_background_output") && case.contains("manage") {
                Some((
                    "stop_background_run",
                    json!({"background_id":self.issued_id}),
                ))
            } else {
                if case.contains("child shell attempt") {
                    self.child_result = last["content"].as_str().map(str::to_owned);
                }
                None
            }
        } else if case.contains("child shell attempt") {
            self.child_definitions = Some(request["tools"].clone());
            Some((
                "shell",
                json!({"command":"exec sleep 30","process_mode":"background","wait_ms":0}),
            ))
        } else if case.contains("delegate capability") {
            Some((
                "delegate_task",
                json!({
                    "title":"Child capability fixture", "task":"F01 fixture child shell attempt",
                    "context":null, "expected_output":"fixture done",
                }),
            ))
        } else if case.contains("launch") {
            self.definitions.push(request["tools"].clone());
            Some((
                "shell",
                json!({
                    "command":"printf '片😀'; exec sleep 30",
                    "process_mode":"background", "wait_ms": if case.contains("manual") { 0 } else { 100 },
                }),
            ))
        } else if case.contains("quick") {
            Some((
                "shell",
                json!({
                    "command":"printf ready; exit 7", "process_mode":"background", "wait_ms":500,
                }),
            ))
        } else if case.contains("timeout") {
            Some((
                "shell",
                json!({
                    "command":"exec sleep 30", "process_mode":"background", "wait_ms":4000, "timeout_ms":100,
                }),
            ))
        } else if case.contains("legacy") {
            Some((
                "shell",
                json!({"command":"printf legacy", "process_mode":"detached"}),
            ))
        } else if case.contains("plan-stop") {
            let id = case.split_whitespace().last().unwrap().trim_matches('"');
            Some(("stop_background_run", json!({"background_id":id})))
        } else if case.contains("manage") {
            Some(("list_background_runs", json!({"limit":1})))
        } else {
            None
        };
        self.responses += 1;
        let response_id = format!("offline-m1-{}", self.responses);
        let mut finish = "stop";
        let delta = match tool {
            Some((name, arguments)) => {
                finish = "tool_calls";
                json!({"role":"assistant","tool_calls":[{
                    "index":0,"id":format!("{response_id}-{name}"),"type":"function",
                    "function":{"name":name,"arguments":arguments.to_string()},
                }]})
            }
            None => json!({"role":"assistant","content":"fixture done"}),
        };
        let chunk = json!({"id":response_id,"object":"chat.completion.chunk","model":"offline-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
        let end = json!({"id":response_id,"object":"chat.completion.chunk","model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":200,"completion_tokens":10,"total_tokens":210}});
        format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n")
    }
}

#[test]
fn formal_host_shell_approval_handoff_management_and_cleanup() {
    let script = Arc::new(Mutex::new(Script::default()));
    let provider = FakeProvider::with_router(Router::new().route(
        "/v1/chat/completions",
        post({
            let script = script.clone();
            move |Json(request): Json<Value>| {
                let response = script.lock().unwrap().answer(&request);
                async move { ([("content-type", "text/event-stream")], response) }
            }
        }),
    ));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    let home = tempfile::Builder::new()
        .prefix("v0332-f01-m1-")
        .tempdir_in(root)
        .unwrap();
    write_config(home.path(), provider.endpoint(), "m1-loopback-fixture");
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let workspace = client.runtime(
        "register_workspace",
        json!({"label":"m1","primary_directory":null,"additional_directories":[]}),
    )["workspace"]["workspace_id"]
        .clone();
    let create = json!({"title":"background fixture", "workspace_id":workspace});
    let session = client.runtime("create_session", create.clone())["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = client.runtime("create_session", create)["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"ask"}),
    );

    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .default_headers(compatibility_headers())
        .timeout(Duration::from_secs(14))
        .build()
        .unwrap();
    let response = http
        .get(format!("{}/events", host.base_url()))
        .bearer_auth(host.access_token())
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    let events = thread::spawn(move || {
        let mut bytes = Vec::new();
        response.take(1024 * 1024).read_to_end(&mut bytes).unwrap();
        bytes
    });

    let running = submit(&mut client, &session, "F01 fixture launch", "build");
    let approval = wait_approval(&mut client, &session);
    assert_eq!(approval["subject"]["process_mode"], "background");
    assert_eq!(approval["subject"]["wait_ms"], 100);
    assert_eq!(approval["subject"]["timeout_ms"], 0);
    assert_eq!(
        client.runtime("list_background_runs", json!({"session_id":session}))["total"],
        0
    );
    client.runtime("decide_approval", json!({"session_id":session,"approval_id":approval["approval_id"],"decision":"allow_session"}));
    client.wait_for_status("background handoff", &session, &running, &["completed"]);
    let shell = result(&detail(&mut client, &session, "shell"));
    assert_eq!(shell["status"], "running");
    assert_eq!(shell["preview"]["content"], "片😀");
    let id = shell["background_id"].as_str().unwrap().to_owned();
    let path = Path::new(shell["log_path"].as_str().unwrap());
    assert!(path.exists());

    let listed = client.runtime(
        "list_background_runs",
        json!({"session_id":session,"background_id":id,"status":"running","limit":1}),
    );
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["runs"][0]["run_id"], running);
    assert_eq!(listed["runs"][0]["log_available"], true);
    assert!(listed["runs"][0].get("command").is_none());
    let exact = client.runtime(
        "list_background_runs",
        json!({"session_id":session,"background_id":id,"include_command":true,"limit":1}),
    );
    assert_eq!(exact["runs"][0]["command"], "printf '片😀'; exec sleep 30");
    assert_eq!(
        client.runtime("list_background_runs", json!({"session_id":other}))["total"],
        0
    );
    let end = client.runtime(
        "read_background_output",
        json!({
            "session_id":session,"background_id":id,"cursor":shell["preview"]["next_cursor"],
        }),
    );
    assert_eq!(end["content"], "");
    assert_eq!(end["next_cursor"], 2);

    let error = |name: &str, session_id: &str| {
        let response = http
            .post(format!("{}/commands", host.base_url()))
            .bearer_auth(host.access_token())
            .json(&json!({
                "request_id":"m1-boundary","command":{"scope":"runtime","payload":{"type":name,
                "payload":{"session_id":session_id,"background_id":id}}},
            }))
            .send()
            .unwrap();
        assert_eq!(response.status(), 404);
        assert_eq!(
            response.json::<Value>().unwrap()["error"]["code"],
            "background_run_not_found"
        );
    };
    error("read_background_output", &other);
    error("stop_background_run", &other);

    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"auto"}),
    );
    let blocked = submit(
        &mut client,
        &session,
        &format!("F01 fixture plan-stop {id}"),
        "plan",
    );
    client.wait_for_status("Plan rejection", &session, &blocked, &["completed"]);
    assert_eq!(
        detail(&mut client, &session, "stop_background_run")["status"],
        "failed"
    );
    assert!(path.exists());

    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"ask"}),
    );
    let managed = submit(&mut client, &session, "F01 fixture manage", "build");
    let stop_approval = wait_approval(&mut client, &session);
    assert_eq!(stop_approval["subject"]["tool_name"], "stop_background_run");
    assert!(path.exists());
    client.runtime(
        "decide_approval",
        json!({"session_id":session,"approval_id":stop_approval["approval_id"],"decision":"allow_once"}),
    );
    client.wait_for_status("management tools", &session, &managed, &["completed"]);
    assert_eq!(
        result(&detail(&mut client, &session, "read_background_output"))["content"],
        "片😀"
    );
    assert_eq!(
        result(&detail(&mut client, &session, "stop_background_run"))["background_id"],
        id
    );
    assert!(!path.exists());
    assert_eq!(
        client.runtime("list_background_runs", json!({"session_id":session}))["total"],
        0
    );
    error("read_background_output", &session);
    error("stop_background_run", &session);

    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"auto"}),
    );
    let fast = submit(&mut client, &session, "F01 fixture quick", "build");
    client.wait_for_status("early exit", &session, &fast, &["completed"]);
    let fast = result(&detail(&mut client, &session, "shell"));
    assert_eq!(fast["status"], "exited");
    assert_eq!(fast["exit_code"], 7);
    assert_eq!(fast["output"]["content"], "ready");
    assert!(fast.get("log_path").is_none());

    let timed = submit(&mut client, &session, "F01 fixture timeout", "build");
    client.wait_for_status("background total timeout", &session, &timed, &["completed"]);
    let timed = result(&detail(&mut client, &session, "shell"));
    assert_eq!(timed["status"], "exited");
    assert_eq!(timed["timed_out"], true);
    assert!(timed.get("background_id").is_none());

    let legacy = submit(&mut client, &session, "F01 fixture legacy", "build");
    client.wait_for_status("legacy start rejection", &session, &legacy, &["completed"]);
    assert_eq!(detail(&mut client, &session, "shell")["status"], "failed");
    let definitions = script.lock().unwrap().definitions[0].clone();
    let schema = &definitions
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "shell")
        .unwrap()["function"]["parameters"];
    assert!(schema.to_string().contains("background"));
    assert!(!schema.to_string().contains("detached"));

    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"ask"}),
    );
    let manual = submit(&mut client, &session, "F01 fixture launch manual", "build");
    client.wait_for_status(
        "second background handoff",
        &session,
        &manual,
        &["completed"],
    );
    let manual = result(&detail(&mut client, &session, "shell"));
    let manual_id = manual["background_id"].as_str().unwrap();
    assert_eq!(
        client.runtime(
            "stop_background_run",
            json!({"session_id":session,"background_id":manual_id}),
        )["background_id"],
        manual_id
    );
    assert!(!Path::new(manual["log_path"].as_str().unwrap()).exists());

    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"auto"}),
    );
    let delegate = submit(
        &mut client,
        &session,
        "F01 fixture delegate capability",
        "build",
    );
    client.wait_for_status("child shell boundary", &session, &delegate, &["completed"]);
    let tasks = client.runtime(
        "list_child_tasks",
        json!({"session_id":session,"parent_run_id":delegate}),
    );
    assert_eq!(tasks["tasks"][0]["status"], "completed");
    let child_tools = script.lock().unwrap().child_definitions.clone().unwrap();
    let child_tools = child_tools.as_array().unwrap();
    let child_shell = child_tools
        .iter()
        .find(|tool| tool["function"]["name"] == "shell")
        .unwrap();
    let child_schema = child_shell["function"]["parameters"].to_string();
    assert!(child_schema.contains("managed"));
    assert!(!child_schema.contains("background"));
    assert!(!child_schema.contains("detached"));
    for name in [
        "list_background_runs",
        "read_background_output",
        "stop_background_run",
    ] {
        assert!(
            !child_tools
                .iter()
                .any(|tool| tool["function"]["name"] == name)
        );
    }
    let child_result = script.lock().unwrap().child_result.clone().unwrap();
    assert!(child_result.contains("unknown variant"), "{child_result}");
    assert_eq!(
        client.runtime("list_background_runs", json!({"session_id":session}))["total"],
        0
    );

    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    let events = String::from_utf8(events.join().unwrap()).unwrap();
    let matching: Vec<_> = events
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .map(|envelope| envelope["event"].clone())
        .filter(|event| {
            event["session_id"] == session
                && (event["background_id"] == id || event["result"]["background_id"] == id)
        })
        .collect();
    assert_eq!(matching.len(), 2);
    assert_eq!(matching[0]["type"], "background_run_changed");
    assert_eq!(matching[1]["type"], "background_run_ended");
    assert!(!matching[1].to_string().contains("片"));
}
