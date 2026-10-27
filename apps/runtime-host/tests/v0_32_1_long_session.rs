//! 按需 Release 性能回归：新建隔离库和合成历史，真实 HTTP/Store/调度配合本机假模型。
//! 不使用用户数据库，不执行历史工具，也不向外部 Provider 发送内容。
mod support;

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use agent_types::*;
use axum::{Json, Router, routing::post};
use rusqlite::Connection;
use serde_json::{Value, json};
use support::{FakeProvider, HostProcess};

#[test]
#[ignore = "explicit Release latency measurement with a large structured history"]
fn long_history_loading_and_input_start_latency() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("long-session-latency-")
        .tempdir_in(root)
        .unwrap();
    let (sent, received) = mpsc::channel();
    let sequence = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = FakeProvider::with_router(Router::new().route("/v1/chat/completions", post(move |Json(_): Json<Value>| {
        let sent = sent.clone();
        let response_id = format!("latency-{}", sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        async move {
            sent.send(Instant::now()).unwrap();
            ([("content-type", "text/event-stream")], concat!(
                "data: {\"id\":\"latency\",\"model\":\"offline-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"latency\",\"model\":\"offline-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            ).replace("latency", &response_id))
        }
    })));
    support::write_config(home.path(), provider.endpoint(), "isolated-latency-key");
    let host = start_host(home.path());
    let mut client = host.connect();
    let long = client.runtime("create_session", json!({"title":"Synthetic long history"}))["session"]["session_id"].as_str().unwrap().to_owned();
    let short = client.runtime(
        "create_session",
        json!({"title":"Independent short session"}),
    )["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    seed_history(home.path(), &long);

    let host = start_host(home.path());
    let mut client = host.connect();
    for round in 0..2 {
        let start = Instant::now();
        let view = client.runtime("get_session_view", json!({"session_id":long}));
        eprintln!(
            "LONG_SESSION phase=view round={round} ms={}",
            start.elapsed().as_millis()
        );
        assert!(
            view["snapshot"]["value"]["conversation"]["has_more"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            view["snapshot"]["value"]["conversation"]["items"]
                .as_array()
                .unwrap()
                .len(),
            30
        );
    }
    // 两个请求同时发送，观察长会话读取对另一个 Session 输入接纳及首次模型调用的影响。
    let mut reading_client = host.connect();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let long = &long;
        let barrier = &barrier;
        let reading = scope.spawn(move || {
            barrier.wait();
            reading_client.runtime("get_session_view", json!({"session_id":long}))
        });
        barrier.wait();
        let start = Instant::now();
        let accepted = client.runtime(
            "submit_input",
            json!({"session_id":short,"message":"short-latency","variant":"build"}),
        );
        eprintln!(
            "LONG_SESSION phase=concurrent_short_accept ms={}",
            start.elapsed().as_millis()
        );
        let requested = received.recv_timeout(Duration::from_secs(15)).unwrap();
        eprintln!(
            "LONG_SESSION phase=concurrent_short_model ms={}",
            requested.duration_since(start).as_millis()
        );
        reading.join().unwrap();
        let run = client.wait_for_input_run(&short, accepted["input_id"].as_str().unwrap());
        client.wait_for_status(
            "short completes",
            &short,
            run["run_id"].as_str().unwrap(),
            &["completed"],
        );
    });
    for round in 0..2 {
        let start = Instant::now();
        let accepted = client.runtime(
            "submit_input",
            json!({"session_id":long,"message":format!("long-latency-{round}"),"variant":"build"}),
        );
        eprintln!(
            "LONG_SESSION phase=accept round={round} ms={}",
            start.elapsed().as_millis()
        );
        let requested = received.recv_timeout(Duration::from_secs(15)).unwrap();
        eprintln!(
            "LONG_SESSION phase=first_model round={round} ms={}",
            requested.duration_since(start).as_millis()
        );
        let run = client.wait_for_input_run(&long, accepted["input_id"].as_str().unwrap());
        client.wait_for_status(
            "long completes",
            &long,
            run["run_id"].as_str().unwrap(),
            &["completed"],
        );
    }
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(host.wait().status.success());
    // 重启后直接发送，不先打开长会话，覆盖首次装配和冷索引。
    let cold_host = start_host(home.path());
    let mut client = cold_host.connect();
    let start = Instant::now();
    let accepted = client.runtime(
        "submit_input",
        json!({"session_id":long,"message":"cold-long-input","variant":"build"}),
    );
    eprintln!(
        "LONG_SESSION phase=cold_accept ms={}",
        start.elapsed().as_millis()
    );
    let requested = received.recv_timeout(Duration::from_secs(15)).unwrap();
    eprintln!(
        "LONG_SESSION phase=cold_first_model ms={}",
        requested.duration_since(start).as_millis()
    );
    let run = client.wait_for_input_run(&long, accepted["input_id"].as_str().unwrap());
    client.wait_for_status(
        "cold long completes",
        &long,
        run["run_id"].as_str().unwrap(),
        &["completed"],
    );
    client.runtime("shutdown_runtime", json!({}));
    drop(client);
    assert!(cold_host.wait().status.success());
    let db = Connection::open_with_flags(
        home.path().join("users/_personal/data/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sessions WHERE role='standard'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT message_count FROM sessions WHERE session_id=?1",
            [&long],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        10006
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM inputs WHERE state='queued'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM runs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM body_appends", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

fn seed_history(home: &Path, session: &str) {
    let personal = home.join("users/_personal");
    let path = personal
        .join("data/sessions")
        .join(session)
        .join("conversation.1.jsonl");
    let mut header = String::new();
    BufReader::new(File::open(&path).unwrap())
        .read_line(&mut header)
        .unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());
    writer.write_all(header.as_bytes()).unwrap();
    // 大量结构化工具结果比单个重复文本更接近实际长会话的 JSON 解码成本。
    let output = json!({"rows": (0..256).map(|index| json!({
        "name":format!("entry-{index}"), "path":"synthetic/file.rs",
        "values":[1,2,3,4,5,6,7,8], "enabled":true,
        "description":"synthetic tool output for decoding performance"
    })).collect::<Vec<_>>()});
    for index in 0..3333 {
        let call = ToolCallId::new(format!("call-{index}")).unwrap();
        let messages = vec![
            ConversationMessage::User(UserMessage {
                id: MessageId::new(format!("user-{index}")).unwrap(),
                origin: UserMessageOrigin::User,
                transcript_visibility: TranscriptVisibility::Visible,
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new(format!("prompt-{index}")).unwrap(),
                    text: "synthetic historical input".into(),
                })],
            }),
            ConversationMessage::Assistant(AssistantMessage {
                id: MessageId::new(format!("assistant-{index}")).unwrap(),
                purpose: Default::default(),
                model: ModelIdentity::new(ProviderId::new("fixture").unwrap(), "fixture"),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: call.clone(),
                    name: ToolName::new("read_file").unwrap(),
                    arguments: json!({"path":"synthetic.txt"}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            }),
            ConversationMessage::Tool(ToolMessage {
                id: MessageId::new(format!("result-{index}")).unwrap(),
                result: ToolResult {
                    call_id: call,
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::json(output.clone()),
                    metadata: None,
                },
            }),
        ];
        serde_json::to_writer(
            &mut writer,
            &json!({"type":"commit","operation_id":format!("fixture-{index}"),"messages":messages}),
        )
        .unwrap();
        writer.write_all(b"\n").unwrap();
    }
    let summary = ConversationMessage::ContextSummary(ContextSummaryMessage {
        retained_user_message_id: None,
        id: MessageId::new("fixture-summary").unwrap(),
        text: "Synthetic history has been compacted; answer the new user input.".into(),
        model: None,
        usage: None,
        compacted_usage: None,
        usage_adjustment: None,
        programmatic_context: None,
    });
    serde_json::to_writer(
        &mut writer,
        &json!({"type":"commit","operation_id":"fixture-summary","messages":[summary]}),
    )
    .unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
    eprintln!(
        "LONG_SESSION synthetic_bytes={} messages=10000",
        path.metadata().unwrap().len()
    );
    // Host 已停止；目标是本用例刚创建的两会话库，应用使用 sessions.message_count 读取摘要。
    let mut db = Connection::open(personal.join("data/runtime.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sessions WHERE role='standard'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let tx = db.transaction().unwrap();
    assert_eq!(tx.execute("UPDATE sessions SET message_count=10000, context_management_mode='local_summary' WHERE session_id=?1 AND message_count=0", [session]).unwrap(), 1);
    tx.commit().unwrap();
    assert_eq!(
        db.query_row(
            "SELECT message_count FROM sessions WHERE session_id=?1",
            [session],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        10000
    );
}

// 可选对比上一份已交付 Host；只能影响测试子进程的可执行文件，不改变 Runtime Home。
fn start_host(home: &Path) -> HostProcess {
    match std::env::var_os("EZ_LONG_SESSION_TEST_HOST") {
        Some(executable) => HostProcess::start_with_executable(home, Path::new(&executable)),
        None => HostProcess::start(home),
    }
}
