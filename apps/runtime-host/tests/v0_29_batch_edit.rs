//! F07 正式 Host 链路：本机受控 Provider、隔离文件和独立 Runtime Home。
#![cfg(unix)]

mod support;

use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use support::{Client, FakeProvider, HostProcess, test_directory, write_config};

fn submit(client: &mut Client, session: &str, key: &str) -> String {
    client.runtime(
        "submit_input",
        json!({"session_id":session,"message":key,"variant":"build","idempotency_key":key}),
    )["run"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn last_detail(client: &mut Client, session: &str) -> Value {
    let page = client.conversation(session);
    for item in page["items"].as_array().unwrap().iter().rev() {
        for segment in item["segments"].as_array().into_iter().flatten() {
            if segment["type"] != "tool_group" {
                continue;
            }
            for tool in segment["tools"].as_array().unwrap().iter().rev() {
                if tool["tool_name"] != "edit_file" {
                    continue;
                }
                return client.runtime("get_tool_detail", json!({"owner":{"type":"main_session","session_id":session},"message_id":item["message_id"],"call_id":tool["call_id"]}))["snapshot"]["value"].clone();
            }
        }
    }
    panic!("edit detail missing: {page}");
}

fn result(detail: &Value) -> Value {
    serde_json::from_str(detail["result_json"].as_str().unwrap()).unwrap()
}

#[test]
fn formal_host_batch_edits_preserve_permissions_cancellation_and_complete_history() {
    let arguments = Arc::new(Mutex::new(Value::Null));
    let ids = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let provider = FakeProvider::with_router(Router::new().route("/v1/chat/completions", post({
        let arguments = arguments.clone(); let ids = ids.clone(); let captured = captured.clone();
        move |Json(body): Json<Value>| {
            let arguments = arguments.clone(); let ids = ids.clone(); let captured = captured.clone();
            async move {
                captured.lock().unwrap().push(body.clone());
                let response_index = ids.fetch_add(1, Ordering::SeqCst);
                let last = body["messages"].as_array().unwrap().last().unwrap();
                let delta = if last["role"] == "tool" {
                    json!({"role":"assistant","content":"done"})
                } else {
                    let index = response_index;
                    json!({"role":"assistant","tool_calls":[{"index":0,"id":format!("batch-{index}"),"type":"function","function":{"name":"edit_file","arguments":serde_json::to_string(&*arguments.lock().unwrap()).unwrap()}}]})
                };
                let finish = if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
                let chunk = json!({"id":format!("offline-{response_index}"),"object":"chat.completion.chunk","model":"offline-model","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let end = json!({"id":format!("offline-{response_index}"),"object":"chat.completion.chunk","model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":finish}],"usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}});
                ([("content-type","text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
            }
        }
    })));
    let home = test_directory();
    let work = test_directory();
    write_config(home.path(), provider.endpoint(), "offline-batch-secret");
    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let workspace = client.runtime(
        "register_workspace",
        json!({"label":"batch","primary_directory":work.path(),"additional_directories":[]}),
    )["workspace"]["workspace_id"]
        .clone();
    let session = client.runtime(
        "create_session",
        json!({"title":"batch","model_selection":null,"workspace_id":workspace}),
    )["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.runtime(
        "set_session_approval_mode",
        json!({"session_id":session,"approval_mode":"auto"}),
    );
    let paths = ["a.txt", "b.txt", "c.txt"].map(|name| work.path().join(name));
    for path in &paths {
        fs::write(path, "old").unwrap();
    }
    let file = |index: usize, old: &str, new: &str| json!({"path":paths[index],"edits":[{"old_string":old,"new_string":new}]});
    *arguments.lock().unwrap() = json!({"files":[file(0,"old","new"),file(1,"old","new")]});
    let run = submit(&mut client, &session, "batch-success");
    client.wait_for_status("batch success", &session, &run, &["completed"]);
    let detail = last_detail(&mut client, &session);
    assert_eq!(result(&detail)["files"].as_array().unwrap().len(), 2);
    assert_eq!(detail["files"].as_array().unwrap().len(), 2);
    assert_eq!(detail["input"]["value"]["type"], "files");
    *arguments.lock().unwrap() = json!({"path":paths[0],"old_string":"new","new_string":"legacy"});
    let run = submit(&mut client, &session, "legacy-success");
    client.wait_for_status("legacy", &session, &run, &["completed"]);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "legacy");
    *arguments.lock().unwrap() =
        json!({"files":[file(0,"legacy","done"),file(1,"missing","new"),file(2,"old","new")]});
    let run = submit(&mut client, &session, "partial-failure");
    client.wait_for_status("partial", &session, &run, &["completed"]);
    let detail = last_detail(&mut client, &session);
    let output = result(&detail);
    assert_eq!(detail["status"], "failed");
    assert_eq!(detail["files"].as_array().unwrap().len(), 1);
    assert_eq!(output["files"][0]["status"], "success");
    assert_eq!(output["files"][1]["status"], "failed");
    assert_eq!(output["files"][2]["status"], "not_executed");
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), "old");
    let download = |resource: &str| {
        reqwest::blocking::Client::new()
            .get(format!(
                "{}/sessions/{session}/messages/{}/resources/{resource}/download",
                host.base_url(),
                detail["message_id"].as_str().unwrap()
            ))
            .bearer_auth(host.access_token())
            .headers(support::compatibility_headers())
            .send()
            .unwrap()
    };
    let response = download(detail["files"][0]["resource_ref_id"].as_str().unwrap());
    assert!(response.status().is_success());
    assert_eq!(response.text().unwrap(), "done");
    // 只有已提交位置有入口；失败与未执行位置不能借索引读取其他文件。
    let missing = format!("tool-file-{}-2", detail["call_id"].as_str().unwrap());
    assert!(!download(&missing).status().is_success());

    let permission_path = home.path().join("users/_personal/permissions.json");
    let permissions = |effect: &str| json!({"schema_version":1,"rules":[{"id":"batch-rule","effect":effect,"variants":["build"],"matcher":{"type":"file","operation":"edit","path_match":"exact","path":paths[1]}}]});
    let reload = |client: &mut Client, value: Value| {
        fs::write(&permission_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            client.runtime("reload_permissions", json!({"session_id":session}))["applied"],
            true
        );
    };
    *arguments.lock().unwrap() =
        json!({"files":[file(0,"done","approved"),file(1,"new","approved")]});
    reload(&mut client, permissions("deny"));
    let run = submit(&mut client, &session, "batch-denied");
    client.wait_for_status("deny", &session, &run, &["completed"]);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "done");
    assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "new");
    reload(&mut client, permissions("ask"));
    let run = submit(&mut client, &session, "batch-recheck");
    let approval = wait_approval(&mut client, &session);
    assert_eq!(approval["subject"]["type"], "files");
    assert_eq!(approval["subject"]["paths"].as_array().unwrap().len(), 2);
    reload(&mut client, permissions("deny"));
    client.runtime(
        "decide_approval",
        json!({"session_id":session,"approval_id":approval["approval_id"],"decision":"allow_once"}),
    );
    client.wait_for_status("approval recheck", &session, &run, &["completed"]);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "done");
    reload(&mut client, permissions("ask"));
    let run = submit(&mut client, &session, "batch-approved");
    let approval = wait_approval(&mut client, &session);
    client.runtime(
        "decide_approval",
        json!({"session_id":session,"approval_id":approval["approval_id"],"decision":"allow_once"}),
    );
    client.wait_for_status("approved", &session, &run, &["completed"]);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "approved");
    assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "approved");
    reload(&mut client, json!({"schema_version":1,"rules":[]}));

    // 显式 Allow 也不能扩大 Plan 的文件修改上限。
    reload(
        &mut client,
        json!({"schema_version":1,"rules":[{"id":"plan-allow","effect":"allow","variants":["plan"],"matcher":{"type":"file","operation":"edit","path":work.path(),"path_match":"recursive"}}]}),
    );
    let plan_run = client.runtime("submit_input",json!({"session_id":session,"message":"plan-denied","variant":"plan","idempotency_key":"plan-denied"}))["run"]["run_id"].as_str().unwrap().to_owned();
    client.wait_for_status("plan boundary", &session, &plan_run, &["completed"]);
    assert_eq!(last_detail(&mut client, &session)["status"], "failed");
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "approved");
    reload(&mut client, json!({"schema_version":1,"rules":[]}));

    // 有界的大文件顺序替换保证运行中取消能落在真实编辑阶段；不阻塞 OS 文件句柄。
    fs::write(&paths[0], "x".repeat(4 * 1024 * 1024)).unwrap();
    let edits = (0..64)
        .map(|index| {
            let (old, new) = if index % 2 == 0 {
                ("x", "y")
            } else {
                ("y", "x")
            };
            json!({"old_string":old,"new_string":new,"replace_all":true})
        })
        .collect::<Vec<_>>();
    *arguments.lock().unwrap() = json!({"files":[{"path":paths[0],"edits":edits},file(1,"approved","cancelled"),file(2,"old","cancelled")]});
    let run = submit(&mut client, &session, "batch-cancelled");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot =
            client.runtime("get_run", json!({"session_id":session,"run_id":run}))["run"].clone();
        if snapshot["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["status"] == "running")
        {
            break;
        }
        assert!(
            Instant::now() < deadline && snapshot["status"] != "completed",
            "tool did not start: {snapshot}"
        );
        thread::sleep(Duration::from_millis(5));
    }
    client.runtime("cancel_run", json!({"session_id":session,"run_id":run}));
    client.wait_for_status("cancelled batch", &session, &run, &["cancelled"]);
    let detail = last_detail(&mut client, &session);
    let output = result(&detail);
    assert_eq!(output["files"].as_array().unwrap().len(), 3);
    assert_eq!(output["files"][2]["status"], "not_executed");
    assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "approved");
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), "old");
    // 大输入正文可以沿既有边界截断，路径投影和结果 JSON 仍必须完整。
    assert_eq!(
        detail["input"]["value"]["paths"].as_array().unwrap().len(),
        3
    );
    let requests = captured.lock().unwrap();
    let schema = &requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "edit_file")
        .unwrap()["function"]["parameters"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["files"]["type"], "array");
    assert_eq!(schema["properties"]["path"]["type"], "string");
    assert!(schema.get("oneOf").is_none());
    assert_eq!(
        requests.len(),
        15,
        "normal cancellation must not start a later model call"
    );
    drop(requests);
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    // 只读核对新建隔离库，不从文件内容反推可靠历史。
    let db = rusqlite::Connection::open_with_flags(
        home.path().join("users/_personal/data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sessions WHERE session_id=?1",
            [&session],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM runs WHERE session_id=?1",
            [&session],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        8
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM pending_tool_exchanges", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT status FROM runs WHERE run_id=?1", [run], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "cancelled"
    );
}

fn wait_approval(client: &mut Client, session: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let approvals =
            client.runtime("list_pending_approvals", json!({"session_id":session}))["approvals"]
                .clone();
        if let Some(approval) = approvals.as_array().unwrap().first() {
            return approval.clone();
        }
        assert!(Instant::now() < deadline, "approval missing");
        thread::sleep(Duration::from_millis(10));
    }
}
