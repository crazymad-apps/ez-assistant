//! 正式 Host 计划管理回归。每个设备使用新建的隔离 Runtime Home。

mod support;

use std::path::Path;

use reqwest::{StatusCode, blocking::Client};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use support::HostProcess;

#[test]
fn schedule_commands_replace_type_persist_and_isolate_devices() {
    let first_home = support::test_directory();
    let other_home = support::test_directory();
    let host = HostProcess::start(first_home.path());
    let mut client = host.connect();
    let other_host = HostProcess::start(other_home.path());
    let mut other = other_host.connect();
    eprintln!(
        "schedule fixture Host API {} and {}",
        host.base_url(),
        other_host.base_url()
    );

    let workspace = client.runtime(
        "register_workspace",
        json!({
            "label":"Scheduled project", "additional_directories":[]
        }),
    );
    let settings = json!({
        "name":"  Daily summary  ", "enabled":true, "workspace_id":workspace["workspace"]["workspace_id"],
        "rule":{"type":"daily","time":"07:30"},
        "action":{
            "type":"agent","instructions":"Prepare a summary","approval_mode":"ask",
            "custom_agent":{"name":"review","source":{"type":"personal"}},
            "model_selection":{"source":"local","provider_instance_id":"fixture","model_id":"model"}
        }
    });
    let created = client.runtime("create_schedule", settings.clone());
    let id = &created["schedule_id"];
    let target = json!({ "schedule_id": id });
    assert_eq!(created["settings"]["name"], "Daily summary");
    assert_eq!(created["settings"]["report_to_controller"], true);
    assert_eq!(
        created["settings"]["workspace_id"],
        settings["workspace_id"]
    );
    assert!(
        created["next_run_at"]
            .as_str()
            .unwrap()
            .contains("T07:30:00")
    );
    assert_eq!(
        client.runtime("list_schedules", json!({}))["page"]["total"],
        1
    );
    assert_eq!(
        client.runtime("list_schedules", json!({"page":{"offset":1,"limit":1}}))["schedules"],
        json!([])
    );
    assert_eq!(
        other.runtime("list_schedules", json!({}))["page"]["total"],
        0
    );
    let old_client = send_command(&host, "list_schedules", json!({}), "0.33.1");
    assert_eq!(old_client.status(), StatusCode::CONFLICT);
    rejected(
        &other_host,
        "get_schedule",
        target.clone(),
        StatusCode::NOT_FOUND,
        "schedule_not_found",
    );
    rejected(
        &other_host,
        "delete_schedule",
        target.clone(),
        StatusCode::NOT_FOUND,
        "schedule_not_found",
    );
    rejected(
        &other_host,
        "create_schedule",
        settings.clone(),
        StatusCode::NOT_FOUND,
        "workspace_not_found",
    );

    let mut script = settings;
    script["enabled"] = json!(false);
    script["report_to_controller"] = json!(false);
    script["action"] = json!({"type":"script","command":"printf 'hello\\n'"});
    let mut missing_report_choice = script.clone();
    missing_report_choice
        .as_object_mut()
        .unwrap()
        .remove("report_to_controller");
    rejected(
        &host,
        "update_schedule",
        json!({"schedule_id":id,"settings":missing_report_choice}),
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let paused = client.runtime(
        "update_schedule",
        json!({"schedule_id":id,"settings":script}),
    );
    assert_eq!(paused["settings"]["action"], script["action"]);
    assert!(paused["next_run_at"].is_null());
    assert_eq!(paused["settings"]["report_to_controller"], false);
    let resumed = client.runtime(
        "set_schedule_enabled",
        json!({"schedule_id":id,"enabled":true}),
    );
    assert_eq!(resumed["settings"]["action"], script["action"]);
    assert_eq!(resumed["settings"]["report_to_controller"], false);
    rejected(
        &host,
        "update_schedule",
        json!({"schedule_id":id,"settings":script,"expected_settings":paused["settings"]}),
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_eq!(client.runtime("get_schedule", target.clone()), resumed);
    assert_eq!(
        client.runtime(
            "set_schedule_enabled",
            json!({"schedule_id":id,"enabled":false})
        ),
        paused
    );

    rejected(
        &host,
        "create_schedule",
        json!({
            "name":"Past", "enabled":true,
            "rule":{"type":"once","date":"2000-01-01","time":"00:00"},
            "action":{"type":"script","command":"echo fixture"}
        }),
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());

    let original = open_read_only(first_home.path());
    assert_eq!(count(&original), 1);
    let action: String = original
        .query_row(
            "SELECT json_extract(settings_json,'$.action') FROM schedules WHERE schedule_id=?1",
            [id.as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&action).unwrap(),
        script["action"]
    );
    drop(original);

    let host = HostProcess::start(first_home.path());
    let mut client = host.connect();
    assert_eq!(client.runtime("get_schedule", target.clone()), paused);
    script["enabled"] = json!(true);
    let resumed = client.runtime(
        "update_schedule",
        json!({"schedule_id":id,"settings":script}),
    );
    assert!(resumed["next_run_at"].is_string());
    assert_eq!(resumed["settings"]["report_to_controller"], false);
    assert_eq!(client.runtime("delete_schedule", target.clone()), target);
    rejected(
        &host,
        "get_schedule",
        target,
        StatusCode::NOT_FOUND,
        "schedule_not_found",
    );
    assert_eq!(
        client.runtime("list_schedules", json!({}))["page"]["total"],
        0
    );
    client.runtime("shutdown_runtime", json!({}));
    other.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    assert!(other_host.wait().status.success());
    assert_eq!(count(&open_read_only(first_home.path())), 0);
    assert_eq!(count(&open_read_only(other_home.path())), 0);
}

fn rejected(host: &HostProcess, kind: &str, payload: Value, expected: StatusCode, code: &str) {
    let response = send_command(host, kind, payload, assistant_protocol::SOFTWARE_VERSION);
    let status = response.status();
    let body: Value = response.json().unwrap();
    assert_eq!(status, expected, "{body}");
    assert_eq!(body["error"]["code"], code, "{body}");
}

fn open_read_only(home: &Path) -> Connection {
    Connection::open_with_flags(
        home.join("users/_personal/data/runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

fn count(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM schedules", [], |row| row.get(0))
        .unwrap()
}

fn send_command(
    host: &HostProcess,
    kind: &str,
    payload: Value,
    client_version: &str,
) -> reqwest::blocking::Response {
    Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap()
        .post(format!("{}/commands", host.base_url()))
        .bearer_auth(host.access_token())
        .header(assistant_protocol::CLIENT_VERSION_HEADER, client_version)
        .header(
            assistant_protocol::MIN_COMPATIBLE_VERSION_HEADER,
            client_version,
        )
        .json(&json!({
           "request_id":"schedule-contract",
           "command":{"scope":"runtime","payload":{"type":kind,"payload":payload}}
        }))
        .send()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn manual_scripts_settle_exit_output_timeout_and_stop() {
    let home = support::test_directory();
    let host = HostProcess::start(home.path());
    eprintln!("M1 verification Host API {}", host.base_url());
    let initial = open_read_only(home.path());
    let baseline: (i64, i64) = initial
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM runs)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(initial);
    let mut client = host.connect();
    let plan = script_plan(
        &mut client,
        "paused manual",
        false,
        json!({"type":"daily","time":"04:37"}),
        "printf 'stdout line\\n'; printf 'stderr line\\n' >&2; exit 7",
    );
    let schedule_id = plan["schedule_id"].as_str().unwrap();
    assert!(plan["next_run_at"].is_null());
    let run = manual(&mut client, schedule_id, "first");
    let execution_id = run["execution"]["execution_id"].as_str().unwrap();
    let result = wait_status(&mut client, execution_id, "failed");
    assert_eq!(result["script_result"]["exit_code"], 7);
    assert_eq!(result["script_result"]["stdout"], "stdout line\n");
    assert_eq!(result["script_result"]["stderr"], "stderr line\n");
    assert!(result["scheduled_for_ms"].is_null());
    let replay = manual(&mut client, schedule_id, "first");
    assert_eq!(replay["type"], "replayed");
    assert_eq!(replay["execution"]["execution_id"], execution_id);
    let still_paused = client.runtime("get_schedule", json!({"schedule_id":schedule_id}));
    assert_eq!(still_paused, plan);

    let project_home = support::test_directory();
    let project = std::fs::canonicalize(project_home.path()).unwrap();
    let project_target = client.runtime("register_workspace", json!({
        "label":"isolated script project","primary_directory":project,"additional_directories":[]
    }));
    let in_project = client.runtime(
        "create_schedule",
        json!({
            "name":"workspace script","enabled":false,"report_to_controller":false,
            "workspace_id":project_target["workspace"]["workspace_id"],
            "rule":{"type":"daily","time":"04:37"},
            "action":{"type":"script","command":"touch workspace-marker"}
        }),
    );
    let run = manual(
        &mut client,
        in_project["schedule_id"].as_str().unwrap(),
        "project",
    );
    let completed = wait_status(
        &mut client,
        run["execution"]["execution_id"].as_str().unwrap(),
        "succeeded",
    );
    assert_eq!(completed["script_result"]["stdout"], "");
    assert_eq!(completed["script_result"]["stderr"], "");
    assert!(project.join("workspace-marker").is_file());

    let capped = script_plan(
        &mut client,
        "bounded output",
        false,
        json!({"type":"daily","time":"04:37"}),
        r"head -c 1100000 /dev/zero | tr '\000' x",
    );
    let capped_run = manual(&mut client, capped["schedule_id"].as_str().unwrap(), "caps");
    let bounded = wait_status(
        &mut client,
        capped_run["execution"]["execution_id"].as_str().unwrap(),
        "succeeded",
    );
    assert_eq!(bounded["script_result"]["truncated"], true);
    assert_eq!(
        bounded["script_result"]["stdout"].as_str().unwrap().len(),
        1024 * 1024
    );

    let command = "printf 'running\\n'; sleep 180 & echo $! > child.pid; wait $!";
    let stopped = script_plan(
        &mut client,
        "stop",
        true,
        json!({"type":"once","date":"2099-01-01","time":"04:37"}),
        command,
    );
    let stopped_plan = stopped["schedule_id"].as_str().unwrap();
    let stop_run = manual(&mut client, stopped_plan, "stop");
    let stop_id = stop_run["execution"]["execution_id"].as_str().unwrap();
    let pid = child_pid(home.path(), stopped_plan);
    let busy = manual(&mut client, stopped_plan, "busy");
    assert_eq!(busy["type"], "busy");
    assert_eq!(busy["execution"]["execution_id"], stop_id);
    assert_eq!(
        client.runtime("get_schedule", json!({"schedule_id":stopped_plan}))["next_run_at"],
        stopped["next_run_at"]
    );
    client.runtime("stop_schedule_execution", json!({"execution_id":stop_id}));
    let cancelled = wait_status(&mut client, stop_id, "cancelled");
    assert_eq!(cancelled["script_result"]["stdout"], "running\n");
    wait_child_gone(pid);

    let timed = script_plan(
        &mut client,
        "timeout",
        false,
        json!({"type":"daily","time":"04:37"}),
        command,
    );
    let timed_plan = timed["schedule_id"].as_str().unwrap();
    let timed_run = manual(&mut client, timed_plan, "timeout");
    let pid = child_pid(home.path(), timed_plan);
    let timeout = wait_status(
        &mut client,
        timed_run["execution"]["execution_id"].as_str().unwrap(),
        "timed_out",
    );
    assert_eq!(timeout["script_result"]["stdout"], "running\n");
    wait_child_gone(pid);

    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    let db = open_read_only(home.path());
    assert_eq!(count(&db), 5);
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM schedule_executions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 5);
    let after: (i64, i64) = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM runs)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        after, baseline,
        "scripts must not add sessions or Agent Runs"
    );
}

#[cfg(unix)]
#[test]
fn timer_runs_without_client_and_skips_once_elapsed_while_host_was_stopped() {
    use chrono::{Local, TimeDelta, Utc};
    use std::{thread, time::Duration};

    let online = support::test_directory();
    let offline = support::test_directory();
    let host = HostProcess::start(online.path());
    let offline_host = HostProcess::start(offline.path());
    eprintln!(
        "M1 timer verification Host APIs {} and {}",
        host.base_url(),
        offline_host.base_url()
    );
    let mut client = host.connect();
    let mut other = offline_host.connect();
    let near = Utc::now() + TimeDelta::seconds(9);
    let due_ms = (near.timestamp() / 60 + 1) * 60 * 1000;
    let local = chrono::DateTime::from_timestamp_millis(due_ms)
        .unwrap()
        .with_timezone(&Local);
    let rule = json!({"type":"once","date":local.format("%Y-%m-%d").to_string(),"time":local.format("%H:%M").to_string()});
    let planned = script_plan(
        &mut client,
        "runs without client",
        true,
        rule.clone(),
        "printf 'timer fired\\n'",
    );
    let timed_agent = client.runtime(
        "create_schedule",
        json!({
            "name":"Automatic agent admission","enabled":true,"report_to_controller":false,
            "rule":rule.clone(),
            "action":{"type":"agent","instructions":"Offline fixture","approval_mode":"ask"},
        }),
    );
    let skipped = script_plan(&mut other, "offline expiry", true, rule, "exit 2");
    other.runtime("shutdown_runtime", json!({}));
    assert!(offline_host.wait().status.success());
    drop(client);
    drop(other);

    let wait = due_ms - Utc::now().timestamp_millis() + 1000;
    thread::sleep(Duration::from_millis(wait.max(0) as u64));
    let mut client = host.connect();
    let id = planned["schedule_id"].as_str().unwrap();
    let history = client.runtime("list_schedule_executions", json!({"schedule_id":id}));
    assert_eq!(history["page"]["total"], 1);
    assert_eq!(history["executions"][0]["scheduled_for_ms"], due_ms);
    assert_eq!(
        client.runtime("get_schedule", json!({"schedule_id":id}))["once_triggered"],
        true
    );
    let execution = history["executions"][0]["execution_id"].as_str().unwrap();
    let done = wait_status(&mut client, execution, "succeeded");
    assert_eq!(done["script_result"]["stdout"], "timer fired\n");
    assert!(client.runtime("get_schedule", json!({"schedule_id":id}))["next_run_at"].is_null());

    let agent_history = client.runtime(
        "list_schedule_executions",
        json!({
            "schedule_id":timed_agent["schedule_id"],
        }),
    );
    assert_eq!(agent_history["page"]["total"], 1);
    assert_eq!(agent_history["executions"][0]["scheduled_for_ms"], due_ms);
    // This isolated Host has no Provider configured, exercising the pre-materialization result.
    let agent = wait_status(
        &mut client,
        agent_history["executions"][0]["execution_id"]
            .as_str()
            .unwrap(),
        "failed",
    );
    assert_eq!(agent["agent_result"]["reason"], "agent_preparation_failed");
    assert!(agent["agent_session_id"].is_null());

    let offline_host = HostProcess::start(offline.path());
    let mut other = offline_host.connect();
    let id = skipped["schedule_id"].as_str().unwrap();
    assert_eq!(
        other.runtime("list_schedule_executions", json!({"schedule_id":id}))["page"]["total"],
        0
    );
    let missed = other.runtime("get_schedule", json!({"schedule_id":id}));
    assert!(missed["next_run_at"].is_null());
    assert_eq!(missed["once_triggered"], false);
    client.runtime("shutdown_runtime", json!({}));
    other.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    assert!(offline_host.wait().status.success());
    for (home, plans, expected) in [(online.path(), 2, 2), (offline.path(), 1, 0)] {
        let db = open_read_only(home);
        assert_eq!(count(&db), plans);
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM schedule_executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, expected);
    }
}

#[cfg(unix)]
fn script_plan(
    client: &mut support::Client,
    name: &str,
    enabled: bool,
    rule: Value,
    command: &str,
) -> Value {
    client.runtime(
        "create_schedule",
        json!({
            "name":name,"enabled":enabled,"report_to_controller":false,
            "rule":rule,"action":{"type":"script","command":command},
        }),
    )
}

#[cfg(unix)]
fn manual(client: &mut support::Client, schedule_id: &str, key: &str) -> Value {
    client.runtime(
        "run_schedule_now",
        json!({"schedule_id":schedule_id,"idempotency_key":key}),
    )
}

#[cfg(unix)]
fn wait_status(client: &mut support::Client, id: &str, expected: &str) -> Value {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let until = Instant::now() + Duration::from_secs(38);
    loop {
        let status = client.runtime("get_schedule_execution", json!({"execution_id":id}));
        if status["status"] == expected {
            return status;
        }
        assert_eq!(
            status["status"], "running",
            "unexpected terminal state: {}",
            status["status"]
        );
        assert!(
            Instant::now() < until,
            "execution {id} did not reach {expected}"
        );
        thread::sleep(Duration::from_millis(80));
    }
}

#[cfg(unix)]
fn child_pid(home: &Path, plan: &str) -> u32 {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let path = home
        .join("users/_personal/data/schedules")
        .join(plan)
        .join("child.pid");
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(pid) = text.trim().parse::<u32>()
            && pid > 1
        {
            return pid;
        }
        assert!(
            Instant::now() < until,
            "script did not publish owned child PID"
        );
        thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(unix)]
fn wait_child_gone(pid: u32) {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let until = Instant::now() + Duration::from_secs(5);
    while std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(Instant::now() < until, "managed child {pid} still alive");
        thread::sleep(Duration::from_millis(40));
    }
}

#[cfg(unix)]
#[test]
fn scheduled_agent_reports_after_background_exit_and_denies_interactive_approval() {
    use axum::{Json, Router, routing::post};
    use std::{
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    let observed = Arc::new(Mutex::new(Vec::new()));
    let provider = support::FakeProvider::with_router(Router::new().route(
        "/v1/chat/completions",
        post({
            let observed = observed.clone();
            let responses = Arc::new(AtomicUsize::new(0));
            move |Json(request): Json<Value>| {
                let response_id = format!("schedule-fixture-{}", responses.fetch_add(1, Ordering::Relaxed));
                let messages = request["messages"].as_array().unwrap();
                let last = messages.last().unwrap();
                let is_user = last["role"] == "user";
                let tool_call = |name, arguments: Value| {
                    json!({"role":"assistant","tool_calls":[{
                        "index":0,"id":format!("scheduled-{name}"),"type":"function",
                        "function":{"name":name,"arguments":arguments.to_string()},
                    }]})
                };
                let summary = messages.iter().any(|message| message["role"] == "system"
                    && message["content"].to_string().contains("The current input is the result of a scheduled task."));
                let (delta, finish) = if summary {
                    assert!(request["tools"].is_null() || request["tools"].as_array().is_some_and(Vec::is_empty));
                    assert!(last["content"].to_string().contains("Agent fixture report"));
                    (json!({"role":"assistant","content":"Controller organized the scheduled result"}), "stop")
                } else if is_user {
                    observed.lock().unwrap().push(request["tools"].clone());
                    let command = r#"printf 'background fixture\n'; n=0; while [ ! -f release ] && [ "$n" -lt 400 ]; do n=$((n+1)); sleep 0.05; done; test -f release && printf complete > done"#;
                    (tool_call("shell", json!({"command":command,"process_mode":"background","wait_ms":0})), "tool_calls")
                } else if last["tool_call_id"] == "scheduled-shell"
                    && !messages.iter().any(|message| message["role"] == "user" && message["content"].to_string().contains("needs approval"))
                {
                    (tool_call("declare_artifact", json!({"action":"declare","path":"report.txt","title":"Report file"})), "tool_calls")
                } else {
                    (json!({"role":"assistant","content":"Agent fixture report"}), "stop")
                };
                let chunk = json!({"id":response_id,"object":"chat.completion.chunk","model":"offline-model",
                    "choices":[{"index":0,"delta":delta,"finish_reason":null}]});
                let end = json!({"id":response_id,"object":"chat.completion.chunk","model":"offline-model",
                    "choices":[{"index":0,"delta":{},"finish_reason":finish}]});
                async move {
                    ([("content-type", "text/event-stream")], format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"))
                }
            }
        }),
    ));
    let home = support::test_directory();
    let project_home = support::test_directory();
    let project = fs::canonicalize(project_home.path()).unwrap();
    fs::write(project.join("report.txt"), "declared fixture").unwrap();
    support::write_config(home.path(), provider.endpoint(), "m2-loopback-fixture");
    let host = HostProcess::start(home.path());
    eprintln!(
        "M2 verification Runtime {} via {}",
        home.path().display(),
        host.base_url()
    );
    let mut client = host.connect();
    let workspace = client.runtime(
        "register_workspace",
        json!({
            "label":"schedule fixture","primary_directory":project,"additional_directories":[],
        }),
    )["workspace"]["workspace_id"]
        .clone();
    let regular = client.runtime(
        "create_session",
        json!({"title":"Control session","workspace_id":workspace}),
    )["session"]["session_id"]
        .clone();
    let baseline = client.runtime("list_sessions", json!({"filter":"all"}));
    let agent_plan = |client: &mut support::Client, instructions, mode, report| {
        client.runtime(
            "create_schedule",
            json!({
                "name":instructions,"enabled":false,"report_to_controller":report,
                "workspace_id":workspace,"rule":{"type":"daily","time":"09:00"},
                "action":{"type":"agent","instructions":instructions,"approval_mode":mode},
            }),
        )["schedule_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let plan = agent_plan(&mut client, "Hidden Agent job", "auto", true);
    let accepted = manual(&mut client, &plan, "agent-first");
    let execution = accepted["execution"]["execution_id"].as_str().unwrap();
    let wait_for_background = |client: &mut support::Client, execution| {
        let until = Instant::now() + Duration::from_secs(12);
        loop {
            let detail =
                client.runtime("get_schedule_execution", json!({"execution_id":execution}));
            assert_eq!(detail["status"], "running", "{detail}");
            if detail["agent_run_status"] == "completed"
                && detail["background_runs"]
                    .as_array()
                    .is_some_and(|runs| runs.len() == 1)
            {
                return detail;
            }
            assert!(
                Instant::now() < until,
                "Agent and detached process did not separate: {detail}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    };
    let running = wait_for_background(&mut client, execution);
    let private = &running["agent_session_id"];
    let background_id = &running["background_runs"][0]["background_id"];
    let background = json!({"execution_id":execution,"background_id":background_id});
    assert!(
        client.runtime("get_schedule_background", background.clone())["command"]
            .as_str()
            .unwrap()
            .contains("background fixture")
    );
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let output = client.runtime("read_schedule_background_output", background.clone());
        if output["content"]
            .as_str()
            .unwrap()
            .contains("background fixture")
        {
            break;
        }
        assert!(Instant::now() < until, "{output}");
        thread::sleep(Duration::from_millis(25));
    }
    rejected(
        &host,
        "read_background_output",
        json!({"session_id":private,"background_id":background_id}),
        StatusCode::NOT_FOUND,
        "session_not_found",
    );
    rejected(
        &host,
        "stop_schedule_background",
        json!({"execution_id":"unrelated","background_id":background_id}),
        StatusCode::NOT_FOUND,
        "background_run_not_found",
    );

    let live = open_read_only(home.path());
    let (session_id, materialization_key): (String, String) = live
        .query_row(
            "SELECT sessions.session_id, sessions.materialization_key
         FROM schedule_executions JOIN sessions USING(session_id) WHERE execution_id=?1",
            [execution],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(private, session_id.as_str());
    assert_eq!(
        materialization_key,
        format!("scheduled-materialization-{execution}")
    );
    drop(live);
    assert_eq!(
        client.runtime("list_sessions", json!({"filter":"all"}))["sessions"],
        baseline["sessions"]
    );
    assert_eq!(client.runtime("search_conversation_history", json!({
        "session_id":regular,"scope":"global","query":"Hidden Agent job","offset":0,"limit":30
    }))["items"], json!([]));
    for (kind, payload) in [
        ("get_session", json!({"session_id":private})),
        (
            "submit_input",
            json!({"session_id":private,"message":"blocked","variant":"build"}),
        ),
        ("list_background_runs", json!({"session_id":private})),
    ] {
        rejected(
            &host,
            kind,
            payload,
            StatusCode::NOT_FOUND,
            "session_not_found",
        );
    }
    assert_eq!(
        manual(&mut client, &plan, "agent-first")["type"],
        "replayed"
    );
    assert_eq!(manual(&mut client, &plan, "agent-overlap")["type"], "busy");
    let mut changed =
        client.runtime("get_schedule", json!({"schedule_id":plan}))["settings"].clone();
    changed["report_to_controller"] = json!(false);
    client.runtime(
        "update_schedule",
        json!({"schedule_id":plan,"settings":changed}),
    );
    fs::rename(
        project.join("report.txt"),
        project.join("report-pending.txt"),
    )
    .unwrap();
    fs::write(project.join("release"), "go").unwrap();
    let terminal = wait_status(&mut client, execution, "succeeded");
    let pending = wait_execution(&mut client, execution, |detail| {
        detail["result_error"].is_string()
    });
    assert_eq!(pending["files"], json!([]));
    assert!(pending["delivery"].is_null());
    assert_eq!(pending["result_finalized"], false);
    fs::rename(
        project.join("report-pending.txt"),
        project.join("report.txt"),
    )
    .unwrap();
    client.runtime("retry_schedule_result", json!({"execution_id":execution}));
    assert_eq!(terminal["agent_result"]["report"], "Agent fixture report");
    assert_eq!(
        terminal["agent_result"]["artifacts"][0]["title"],
        "Report file"
    );
    assert_eq!(
        terminal["agent_result"]["background_exits"][0]["exit_code"],
        0
    );
    assert_eq!(
        fs::read_to_string(project.join("done")).unwrap(),
        "complete"
    );

    let delivered = wait_execution(&mut client, execution, |detail| {
        detail["delivery"].is_object() && detail["agent_session_id"].is_null()
    });
    assert_eq!(delivered["settings"]["report_to_controller"], true);
    assert!(delivered["result_finalized"].as_bool().unwrap());
    let delivery = delivered["delivery"].clone();
    let retried = client.runtime("retry_schedule_result", json!({"execution_id":execution}));
    assert_eq!(retried["delivery"], delivery);
    let file_id = delivered["files"][0]["file_id"].as_str().unwrap();
    assert_eq!(
        download_result(&host, execution, file_id).text().unwrap(),
        "declared fixture"
    );
    let messages = client.conversation(delivery["session_id"].as_str().unwrap());
    assert!(
        messages["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["message_id"] == delivery["message_id"]
                && message["source"]["type"] == "schedule_report"
                && message["source"]["files"] == delivered["files"]),
        "{messages}"
    );

    let report_deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let messages = client.conversation(delivery["session_id"].as_str().unwrap());
        let summaries = messages["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| {
                message["type"] == "assistant"
                    && message["segments"]
                        .to_string()
                        .contains("Controller organized the scheduled result")
            })
            .count();
        if summaries == 1 {
            break;
        }
        assert!(Instant::now() < report_deadline, "{messages}");
        thread::sleep(Duration::from_millis(25));
    }

    fs::remove_file(project.join("release")).unwrap();
    let stopped = manual(&mut client, &plan, "agent-stop");
    let stop_id = stopped["execution"]["execution_id"].as_str().unwrap();
    let second = wait_for_background(&mut client, stop_id);
    assert_ne!(second["agent_session_id"], *private);
    let mut changed =
        client.runtime("get_schedule", json!({"schedule_id":plan}))["settings"].clone();
    changed["report_to_controller"] = json!(true);
    client.runtime(
        "update_schedule",
        json!({"schedule_id":plan,"settings":changed}),
    );
    client.runtime("stop_schedule_execution", json!({"execution_id":stop_id}));
    let stopped = wait_status(&mut client, stop_id, "cancelled");
    assert_eq!(
        stopped["agent_result"]["background_exits"][0]["stopped"],
        true
    );

    let undelivered = wait_execution(&mut client, stop_id, |detail| {
        detail["agent_session_id"].is_null()
            && detail["files"]
                .as_array()
                .is_some_and(|files| files.len() == 1)
    });
    assert!(undelivered["delivery"].is_null());
    assert_eq!(undelivered["settings"]["report_to_controller"], false);
    let unsent_file = undelivered["files"][0]["file_id"].as_str().unwrap();
    assert_eq!(
        download_result(&host, stop_id, unsent_file).status(),
        StatusCode::OK
    );

    let ask = agent_plan(&mut client, "Agent needs approval", "ask", false);
    let blocked = manual(&mut client, &ask, "agent-ask");
    let blocked_id = blocked["execution"]["execution_id"].as_str().unwrap();
    let blocked = wait_status(&mut client, blocked_id, "failed");
    assert_eq!(blocked["agent_result"]["reason"], "agent_approval_required");
    assert_eq!(blocked["agent_result"]["background_exits"], json!([]));
    let blocked = wait_execution(&mut client, blocked_id, |detail| {
        detail["agent_session_id"].is_null()
    });
    assert!(blocked["delivery"].is_null());
    let definitions = observed.lock().unwrap();
    assert_eq!(definitions.len(), 3);
    assert!(definitions.iter().all(|tools| {
        tools.as_array().unwrap().iter().all(|tool| {
            !matches!(
                tool["function"]["name"].as_str(),
                Some("ask_user" | "manage_schedules")
            )
        })
    }));
    drop(definitions);

    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    let db = open_read_only(home.path());
    assert_eq!(count(&db), 2);
    let counts: (i64, i64, i64, i64) = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM schedule_executions),
                (SELECT COUNT(*) FROM sessions JOIN schedule_executions USING(session_id)),
                (SELECT COUNT(*) FROM inputs JOIN schedule_executions USING(session_id)),
                (SELECT COUNT(*) FROM runs JOIN schedule_executions USING(session_id))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    eprintln!("M2 isolated SQLite counts (executions, sessions, inputs, runs): {counts:?}");
    assert_eq!(counts, (3, 0, 0, 0));
    drop(db);

    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    let recovered = wait_status(&mut client, execution, "succeeded");
    assert_eq!(recovered["agent_result"], terminal["agent_result"]);
    assert_eq!(recovered["delivery"], delivery);
    let deleting = manual(&mut client, &plan, "agent-delete");
    let deleting_id = deleting["execution"]["execution_id"].as_str().unwrap();
    let active = wait_for_background(&mut client, deleting_id);
    let task_root = home
        .path()
        .join("users/_personal/data/sessions")
        .join(active["agent_session_id"].as_str().unwrap());
    let process_log =
        std::path::PathBuf::from(active["background_runs"][0]["log_path"].as_str().unwrap());
    client.runtime("delete_schedule", json!({"schedule_id":plan}));
    assert!(!task_root.exists());
    assert!(!process_log.exists());
    assert_eq!(
        client.conversation(delivery["session_id"].as_str().unwrap())["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"]["type"] == "schedule_report")
            .count(),
        1
    );
    rejected(
        &host,
        "get_schedule_execution",
        json!({"execution_id":execution}),
        StatusCode::NOT_FOUND,
        "schedule_execution_not_found",
    );
    assert_eq!(
        download_result(&host, execution, file_id).text().unwrap(),
        "declared fixture"
    );
    assert_eq!(
        download_result(&host, stop_id, unsent_file).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fs::read_to_string(project.join("report.txt")).unwrap(),
        "declared fixture"
    );
    rejected(
        &host,
        "get_session",
        json!({"session_id":private}),
        StatusCode::NOT_FOUND,
        "session_not_found",
    );
    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
}

#[cfg(unix)]
fn wait_execution(client: &mut support::Client, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let execution = client.runtime("get_schedule_execution", json!({"execution_id":id}));
        if ready(&execution) {
            return execution;
        }
        assert!(
            Instant::now() < deadline,
            "execution {id} did not complete postprocessing: {execution}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn download_result(host: &HostProcess, id: &str, file_id: &str) -> reqwest::blocking::Response {
    Client::builder()
        .no_proxy()
        .default_headers(support::compatibility_headers())
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap()
        .get(format!(
            "{}/schedule-results/{id}/files/{file_id}/download",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .send()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn scheduled_script_delivery_uses_frozen_settings_and_deletion_recovers_processes() {
    use std::{
        fs, thread,
        time::{Duration, Instant},
    };

    let home = support::test_directory();
    let host = HostProcess::start(home.path());
    eprintln!(
        "M3 script Runtime {} via {}",
        home.path().display(),
        host.base_url()
    );
    let mut client = host.connect();
    let plan = client.runtime("create_schedule", json!({
        "name":"Script delivery","enabled":false,"report_to_controller":true,
        "rule":{"type":"daily","time":"09:00"},
        "action":{"type":"script","command":"printf 'plain output\\n'; printf 'plain error\\n' >&2; n=0; while [ ! -f release ] && [ \"$n\" -lt 300 ]; do n=$((n+1)); sleep 0.05; done; test -f release"}
    }));
    let schedule = plan["schedule_id"].as_str().unwrap();
    let received = manual(&mut client, schedule, "script-send");
    let sent = received["execution"]["execution_id"].as_str().unwrap();
    let release = home
        .path()
        .join("users/_personal/data/schedules")
        .join(schedule)
        .join("release");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !release.parent().unwrap().is_dir() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(25));
    }
    let mut settings = plan["settings"].clone();
    settings["report_to_controller"] = json!(false);
    client.runtime(
        "update_schedule",
        json!({"schedule_id":schedule,"settings":settings}),
    );
    fs::write(&release, "").unwrap();
    let completed = wait_execution(&mut client, sent, |detail| detail["delivery"].is_object());
    assert_eq!(completed["status"], "succeeded");
    assert_eq!(completed["settings"]["report_to_controller"], true);
    assert_eq!(completed["script_result"]["stdout"], "plain output\n");
    assert_eq!(completed["script_result"]["stderr"], "plain error\n");
    let delivery = &completed["delivery"];
    let controller = delivery["session_id"].as_str().unwrap();
    let get_report_count = |client: &mut support::Client| {
        client.conversation(controller)["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"]["type"] == "schedule_report")
            .count()
    };
    assert_eq!(get_report_count(&mut client), 1);
    settings["action"]["command"] = json!("exit 9");
    client.runtime(
        "update_schedule",
        json!({"schedule_id":schedule,"settings":settings}),
    );
    let failed = manual(&mut client, schedule, "script-local");
    let failed_id = failed["execution"]["execution_id"].as_str().unwrap();
    let failed = wait_status(&mut client, failed_id, "failed");
    assert_eq!(failed["script_result"]["exit_code"], 9);
    assert!(failed["delivery"].is_null());

    let running = script_plan(
        &mut client,
        "Running delete",
        false,
        json!({"type":"daily","time":"09:00"}),
        "sleep 20 & printf '%s' \"$!\" > child.pid; wait",
    );
    let running_id = running["schedule_id"].as_str().unwrap();
    manual(&mut client, running_id, "script-delete");
    let pid = child_pid(home.path(), running_id);
    assert_eq!(
        client.runtime("delete_schedule", json!({"schedule_id":running_id}))["schedule_id"],
        running_id
    );
    wait_child_gone(pid);
    assert!(
        !home
            .path()
            .join("users/_personal/data/schedules")
            .join(running_id)
            .exists()
    );

    let interrupted = script_plan(
        &mut client,
        "Interrupted delete",
        false,
        json!({"type":"daily","time":"09:00"}),
        "sleep 20 & printf '%s' \"$!\" > child.pid; wait",
    );
    let interrupted_id = interrupted["schedule_id"].as_str().unwrap();
    let running = manual(&mut client, interrupted_id, "script-interrupted");
    let lost = running["execution"]["execution_id"].as_str().unwrap();
    let pid = child_pid(home.path(), interrupted_id);
    let identity = home
        .path()
        .join("users/_personal/background-shell")
        .join(lost)
        .join("identity.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !identity.is_file() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(25));
    }
    host.kill();

    let host = HostProcess::start(home.path());
    let mut client = host.connect();
    wait_child_gone(pid);
    let interrupted_execution = wait_status(&mut client, lost, "outcome_unknown");
    assert_eq!(
        interrupted_execution["script_result"]["reason"],
        "host_interrupted"
    );
    assert!(interrupted_execution["delivery"].is_null());
    assert_eq!(
        client.runtime("delete_schedule", json!({"schedule_id":interrupted_id}))["schedule_id"],
        interrupted_id
    );
    assert_eq!(get_report_count(&mut client), 1);
    assert_eq!(
        wait_status(&mut client, sent, "succeeded")["delivery"],
        *delivery
    );
    client.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    let db = open_read_only(home.path());
    assert_eq!(count(&db), 1);
    for (table, count) in [("schedule_executions", 2), ("schedule_deliveries", 1)] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            count
        );
    }
}
