#![allow(dead_code)]

use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use axum::{Json, Router, response::IntoResponse, routing::post};
use reqwest::blocking::Client as HttpClient;
use serde_json::{Value, json};
use tokio::sync::oneshot;

pub struct FakeProvider {
    endpoint: String,
    shutdown: Option<oneshot::Sender<()>>,
    owner: Option<thread::JoinHandle<()>>,
}

impl FakeProvider {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        listener
            .set_nonblocking(true)
            .expect("nonblocking fake provider");
        let address = listener.local_addr().expect("fake provider address");
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let owner = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fake Provider runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("fake Provider Tokio listener");
                let app = Router::new().route("/v1/chat/completions", post(provider_response));
                tokio::select! {
                    result = axum::serve(listener, app) => result.expect("serve fake Provider"),
                    _ = shutdown_receiver => {}
                }
            });
        });
        Self {
            endpoint: format!("http://{address}/v1"),
            shutdown: Some(shutdown),
            owner: Some(owner),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(owner) = self.owner.take() {
            owner.join().expect("join fake provider");
        }
    }
}

async fn provider_response(Json(body): Json<Value>) -> impl IntoResponse {
    let body_text = serde_json::to_string(&body).expect("serialize fake Provider request");
    let case = latest_case(&body_text);
    let current_turn_has_tool_result = current_turn_has_tool_result(&body);
    if matches!(case, "BLOCK_FOR_RESTART" | "CANCEL_CASE") {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let response = if case == "DELEGATE_PARALLEL_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !current_turn_has_tool_result
    {
        parallel_delegate_task_tool_response(tool_exchange_number(&body))
    } else if case == "DELEGATE_BLOCK_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !current_turn_has_tool_result
    {
        blocking_delegate_task_tool_response(tool_exchange_number(&body))
    } else if case == "DELEGATE_CASE"
        && has_tool_definition(&body, "delegate_task")
        && !current_turn_has_tool_result
    {
        delegate_task_tool_response(tool_exchange_number(&body))
    } else if case == "TOOL_CASE" && !current_turn_has_tool_result {
        directory_list_tool_response(tool_exchange_number(&body))
    } else if case == "FILE_REFERENCE_CASE" && !current_turn_has_tool_result {
        file_read_tool_response(
            attached_file_path(&body).expect("File References request contains a readable path"),
            tool_exchange_number(&body),
        )
    } else {
        let response_id = if matches!(case, "FILE_REFERENCE_CASE" | "TOOL_CASE") {
            format!("text-{case}-{}", tool_exchange_number(&body))
        } else {
            format!("text-{case}")
        };
        let text = if case == "REPLACEMENT_CASE" {
            "replacement answer"
        } else if case == "TOOL_CASE" {
            "tool answer"
        } else if case == "FILE_REFERENCE_CASE" && body_text.contains("attachment-tool-token-91") {
            "file tool verified"
        } else if case == "FILE_REFERENCE_CASE" {
            "file tool result missing"
        } else if case == "QUEUED_AFTER_RESTART" {
            "resumed answer"
        } else {
            "offline answer"
        };
        format!(
            "data: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{text:?}}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":120,\"completion_tokens\":20,\"total_tokens\":140,\"prompt_tokens_details\":{{\"cached_tokens\":80}}}}}}\n\ndata: [DONE]\n\n"
        )
    };
    ([("content-type", "text/event-stream")], response)
}

/// 只观察最近一条 User Message 之后的消息，避免历史 Tool Result 让新的 Run
/// 被 fake Provider 误判为已经完成当前工具调用。
fn current_turn_has_tool_result(body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    let Some(last_user) = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return false;
    };
    messages[last_user + 1..]
        .iter()
        .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
}

fn tool_exchange_number(body: &Value) -> usize {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .count()
        + usize::from(!current_turn_has_tool_result(body))
}

fn has_tool_definition(body: &Value, expected_name: &str) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                == Some(expected_name)
        })
}

fn delegate_task_tool_response(exchange_number: usize) -> String {
    let proposal = json!({
        "id": format!("delegate-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-delegate-{exchange_number}"),
                    "type": "function",
                    "function": {
                        "name": "delegate_task",
                        "arguments": "{\"title\":\"Offline child\",\"task\":\"Return one final answer.\"}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("delegate-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn parallel_delegate_task_tool_response(exchange_number: usize) -> String {
    delegate_tool_response(
        exchange_number,
        &[
            ("Offline child A", "Return child A result."),
            ("Offline child B", "Return child B result."),
        ],
    )
}

fn blocking_delegate_task_tool_response(exchange_number: usize) -> String {
    delegate_tool_response(
        exchange_number,
        &[("Interrupted child", "BLOCK_FOR_RESTART")],
    )
}

fn delegate_tool_response(exchange_number: usize, tasks: &[(&str, &str)]) -> String {
    let tool_calls = tasks
        .iter()
        .enumerate()
        .map(|(index, (title, task))| {
            json!({
                "index": index,
                "id": format!("call-delegate-{exchange_number}-{index}"),
                "type": "function",
                "function": {
                    "name": "delegate_task",
                    "arguments": serde_json::to_string(&json!({
                        "title": title,
                        "task": task,
                    })).expect("serialize delegate arguments")
                }
            })
        })
        .collect::<Vec<_>>();
    let proposal = json!({
        "id": format!("delegate-batch-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "tool_calls": tool_calls },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("delegate-batch-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn directory_list_tool_response(exchange_number: usize) -> String {
    let proposal = json!({
        "id": format!("tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-list-directory-{exchange_number}"),
                    "type": "function",
                    "function": {
                        "name": "list_directory",
                        "arguments": "{\"path\":\".\"}"
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn latest_case(body: &str) -> &'static str {
    [
        "FIRST_CASE",
        "BLOCK_FOR_RESTART",
        "QUEUED_AFTER_RESTART",
        "TOOL_CASE",
        "CANCEL_CASE",
        "REPLACEMENT_CASE",
        "FILE_REFERENCE_CASE",
        "DELEGATE_CASE",
        "DELEGATE_PARALLEL_CASE",
        "DELEGATE_BLOCK_CASE",
    ]
    .into_iter()
    .filter_map(|marker| body.rfind(marker).map(|position| (position, marker)))
    .max_by_key(|(position, _)| *position)
    .map_or("DEFAULT_CASE", |(_, marker)| marker)
}

fn attached_file_path(body: &Value) -> Option<&str> {
    body.get("messages")?
        .as_array()?
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .find_map(|text| {
            let start = text.find("<path>")? + "<path>".len();
            let end = text[start..].find("</path>")? + start;
            Some(&text[start..end])
        })
}

fn file_read_tool_response(path: &str, exchange_number: usize) -> String {
    let arguments =
        serde_json::to_string(&json!({ "path": path })).expect("serialize read_file arguments");
    let proposal = json!({
        "id": format!("file-tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": format!("call-read-file-{exchange_number}"),
                    "type": "function",
                    "function": { "name": "read_file", "arguments": arguments }
                }]
            },
            "finish_reason": null
        }]
    });
    let finish = json!({
        "id": format!("file-tool-{exchange_number}"),
        "model": "offline-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }
    });
    format!("data: {proposal}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

pub fn write_config(runtime_home: &Path, endpoint: &str, api_key: &str) {
    fs::create_dir_all(runtime_home).expect("create runtime home");
    let document = format!(
        r#"schema_version = 1
default_model = "fixture"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[models.fixture]
protocol = "chat_completions"
provider = "fixture"
endpoint = "{endpoint}"
model = "offline-model"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096

[models.alternate]
protocol = "chat_completions"
provider = "fixture"
endpoint = "{endpoint}"
model = "offline-alternate"
api_key = "{api_key}"
context_window_tokens = 8192
max_output_tokens = 4096
"#
    );
    fs::write(runtime_home.join("config.toml"), document).expect("write test config");
}

pub struct HostProcess {
    child: Option<Child>,
    base_url: String,
    access_token: String,
}

impl HostProcess {
    pub fn start(runtime_home: &Path) -> Self {
        Self::start_with_options(runtime_home, false)
    }

    #[cfg(feature = "web-demo")]
    pub fn start_web_demo(runtime_home: &Path) -> Self {
        Self::start_with_options(runtime_home, true)
    }

    fn start_with_options(runtime_home: &Path, web_demo: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"));
        command
            .arg("serve")
            .arg("--runtime-home")
            .arg(runtime_home)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(feature = "web-demo")]
        if web_demo {
            command.arg("--web-demo");
        }
        #[cfg(not(feature = "web-demo"))]
        let _ = web_demo;
        let mut child = command.spawn().expect("spawn Runtime Host");
        let (base_url, access_token) = wait_until_ready(runtime_home, &mut child);
        Self {
            child: Some(child),
            base_url,
            access_token,
        }
    }

    pub fn connect(&self) -> Client {
        Client::connect(self.base_url.clone(), self.access_token.clone())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn kill(mut self) -> Output {
        let mut child = self.child.take().expect("owned Runtime Host");
        child.kill().expect("kill Runtime Host");
        child.wait_with_output().expect("wait killed Host")
    }

    pub fn wait(mut self) -> Output {
        self.child
            .take()
            .expect("owned Runtime Host")
            .wait_with_output()
            .expect("wait Runtime Host")
    }
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct Client {
    http: HttpClient,
    base_url: String,
    access_token: String,
    next_request: u64,
}

impl Client {
    fn connect(base_url: String, access_token: String) -> Self {
        Self {
            http: HttpClient::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(12))
                .build()
                .expect("HTTP client"),
            base_url,
            access_token,
            next_request: 1,
        }
    }

    pub fn runtime(&mut self, command_type: &str, payload: Value) -> Value {
        let result = self.request(json!({
            "scope": "runtime",
            "payload": {
                "type": command_type,
                "payload": payload
            }
        }));
        assert_eq!(result["scope"], "runtime");
        assert_eq!(result["payload"]["type"], command_type);
        result["payload"]["payload"].clone()
    }

    pub fn conversation(&mut self, session_id: &str) -> Value {
        let result = self.request(json!({
            "scope": "conversation_snapshot",
            "payload": { "session_id": session_id }
        }));
        assert_eq!(result["scope"], "conversation_snapshot");
        result["payload"]["conversation"].clone()
    }

    pub fn child_conversation(&mut self, session_id: &str, child_task_id: &str) -> Value {
        let result = self.request(json!({
            "scope": "child_task_conversation_snapshot",
            "payload": {
                "session_id": session_id,
                "child_task_id": child_task_id
            }
        }));
        assert_eq!(result["scope"], "child_task_conversation_snapshot");
        result["payload"]["conversation"].clone()
    }

    pub fn wait_for_status(
        &mut self,
        context: &str,
        session_id: &str,
        run_id: &str,
        expected: &[&str],
    ) -> Value {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let run = self.runtime(
                "get_run",
                json!({ "session_id": session_id, "run_id": run_id }),
            )["run"]
                .clone();
            if expected.contains(&run["status"].as_str().expect("run status")) {
                return run;
            }
            let status = run["status"].as_str().expect("run status");
            assert!(
                !matches!(status, "completed" | "failed" | "cancelled" | "interrupted"),
                "{context}: Run {run_id} reached unexpected terminal state: {run}"
            );
            assert!(
                Instant::now() < deadline,
                "{context}: Run {run_id} did not reach {expected:?}; last snapshot: {run}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn request(&mut self, command: Value) -> Value {
        let request_id = format!("request-{}", self.next_request);
        self.next_request += 1;
        let response = self
            .http
            .post(format!("{}/commands", self.base_url))
            .bearer_auth(&self.access_token)
            .json(&json!({
                "request_id": request_id,
                "command": command
            }))
            .send()
            .expect("send Runtime command");
        let status = response.status();
        let body: Value = response.json().expect("decode Runtime command response");
        assert!(
            status.is_success(),
            "Runtime command failed ({status}): {body}"
        );
        assert_eq!(body["request_id"], request_id);
        body["result"].clone()
    }
}

fn wait_until_ready(runtime_home: &Path, child: &mut Child) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(8);
    let discovery_path = runtime_home.join("run/runtime.json");
    let http = HttpClient::builder()
        .connect_timeout(Duration::from_millis(200))
        .timeout(Duration::from_millis(500))
        .build()
        .expect("readiness client");
    loop {
        if let Ok(bytes) = fs::read(&discovery_path)
            && let Ok(discovery) = serde_json::from_slice::<Value>(&bytes)
            && let (Some(base_url), Some(access_token)) = (
                discovery["address"].as_str(),
                discovery["access_token"].as_str(),
            )
            && http
                .get(format!("{base_url}/health"))
                .bearer_auth(access_token)
                .send()
                .is_ok_and(|response| response.status().is_success())
        {
            return (base_url.to_owned(), access_token.to_owned());
        }
        if let Some(status) = child.try_wait().expect("poll Runtime Host") {
            panic!("Runtime Host exited before ready: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "Runtime Host did not become ready"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
