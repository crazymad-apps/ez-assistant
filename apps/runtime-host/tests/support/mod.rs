use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use axum::{Json, Router, response::IntoResponse, routing::post};
use serde_json::{Value, json};
use tokio::sync::oneshot;

const PROTOCOL_VERSION: u32 = 1;

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
    let body = serde_json::to_string(&body).expect("serialize fake Provider request");
    let case = latest_case(&body);
    if matches!(case, "BLOCK_FOR_RESTART" | "CANCEL_CASE") {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let response = if case == "TOOL_CASE" && !body.contains("\"role\":\"tool\"") {
        concat!(
            "data: {\"id\":\"tool-1\",\"model\":\"offline-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call-echo-1\",\"type\":\"function\",\"function\":{\"name\":\"echo_text\",\"arguments\":\"{\\\"text\\\":\\\"offline echo\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"tool-1\",\"model\":\"offline-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        )
        .to_owned()
    } else {
        let response_id = format!("text-{case}");
        let text = if case == "REPLACEMENT_CASE" {
            "replacement answer"
        } else if case == "TOOL_CASE" {
            "tool answer"
        } else if case == "QUEUED_AFTER_RESTART" {
            "resumed answer"
        } else {
            "offline answer"
        };
        format!(
            "data: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{text:?}}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":{response_id:?},\"model\":\"offline-model\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        )
    };
    ([("content-type", "text/event-stream")], response)
}

fn latest_case(body: &str) -> &'static str {
    [
        "FIRST_CASE",
        "BLOCK_FOR_RESTART",
        "QUEUED_AFTER_RESTART",
        "TOOL_CASE",
        "CANCEL_CASE",
        "REPLACEMENT_CASE",
    ]
    .into_iter()
    .filter_map(|marker| body.rfind(marker).map(|position| (position, marker)))
    .max_by_key(|(position, _)| *position)
    .map_or("DEFAULT_CASE", |(_, marker)| marker)
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
    socket: PathBuf,
}

impl HostProcess {
    pub fn start(runtime_home: &Path, socket: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_ez-assistant-runtime"))
            .arg("serve")
            .arg("--runtime-home")
            .arg(runtime_home)
            .arg("--socket")
            .arg(socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn Runtime Host");
        let mut host = Self {
            child: Some(child),
            socket: socket.to_owned(),
        };
        host.wait_until_ready();
        host
    }

    pub fn connect(&self) -> Client {
        Client::connect(&self.socket)
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

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            if let Some(status) = self
                .child
                .as_mut()
                .expect("owned Runtime Host")
                .try_wait()
                .expect("poll Runtime Host")
            {
                panic!("Runtime Host exited before ready: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "Runtime Host did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
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
    stream: UnixStream,
    next_request: u64,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let stream = UnixStream::connect(socket).expect("connect Runtime Host");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("client read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("client write timeout");
        let mut client = Self {
            stream,
            next_request: 1,
        };
        client.write(&json!({
            "type": "hello",
            "protocol_version": PROTOCOL_VERSION,
            "client_name": "v0.10.0-offline-acceptance"
        }));
        let hello = client.read();
        assert_eq!(hello["type"], "hello_ack");
        assert_eq!(hello["protocol_version"], PROTOCOL_VERSION);
        client
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
        self.write(&json!({
            "type": "request",
            "request_id": request_id,
            "command": command
        }));
        loop {
            let frame = self.read();
            match frame["type"].as_str() {
                Some("response") if frame["request_id"] == request_id => {
                    return frame["result"].clone();
                }
                Some("error") if frame["request_id"] == request_id => {
                    panic!("Runtime command failed: {}", frame["error"]);
                }
                Some("event") => {}
                other => panic!("unexpected server frame: {other:?}"),
            }
        }
    }

    fn write(&mut self, value: &Value) {
        let bytes = serde_json::to_vec(value).expect("encode client frame");
        let length = u32::try_from(bytes.len()).expect("frame length");
        self.stream
            .write_all(&length.to_be_bytes())
            .expect("write frame header");
        self.stream.write_all(&bytes).expect("write frame body");
        self.stream.flush().expect("flush client frame");
    }

    fn read(&mut self) -> Value {
        let mut header = [0_u8; 4];
        self.stream
            .read_exact(&mut header)
            .expect("read frame header");
        let length = u32::from_be_bytes(header) as usize;
        assert!((1..=1024 * 1024).contains(&length));
        let mut bytes = vec![0_u8; length];
        self.stream.read_exact(&mut bytes).expect("read frame body");
        serde_json::from_slice(&bytes).expect("decode server frame")
    }
}
