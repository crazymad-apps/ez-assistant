//! 真实 Host + PTY + WebSocket，所有命令只操作临时夹具；不访问已安装应用或真实 Provider。
#![cfg(unix)]
mod support;

use serde_json::{Value, json};
use std::{
    fs,
    net::TcpStream,
    path::Path,
    thread,
    time::{Duration, Instant},
};
use support::HostProcess;
use tungstenite::{Message, WebSocket, client::IntoClientRequest, stream::MaybeTlsStream};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn socket(host: &HostProcess, cookie: Option<&str>) -> Socket {
    let mut request = format!(
        "{}/user-terminals/socket",
        host.base_url().replace("http:", "ws:")
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("Origin", host.base_url().parse().unwrap());
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert("Cookie", cookie.parse().unwrap());
    } else {
        request
            .headers_mut()
            .insert("Origin", "tauri://localhost".parse().unwrap());
    }
    let (socket, _) = tungstenite::connect(request).unwrap();
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
    }
    socket
}
fn control(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .unwrap();
}
fn open(socket: &mut Socket, token: Option<&str>, source: &Value) -> Value {
    control(
        socket,
        json!({"type":"open", "bearer":token, "source":source, "size":{"cols":80,"rows":24}}),
    );
    let result = notice(socket);
    if result["type"] == "created" {
        // Created 表示 PTY 已创建；登录 Shell 仍可初始化 termios 并清空预先输入。
        // 交互用例等到真实提示符，再发送命令；无需产品识别 Shell 私有的启动脚本。
        let mut output = Vec::new();
        loop {
            match socket.read().unwrap() {
                Message::Binary(bytes) => {
                    output.extend(bytes);
                    control(socket, json!({"type":"ack"}));
                    if output.ends_with(b"% ")
                        || output.ends_with(b"$ ")
                        || output.ends_with(b"# ")
                        || output.windows(8).any(|bytes| bytes == b"\x1b[?2004h")
                    {
                        break;
                    }
                }
                Message::Ping(_) => socket.flush().unwrap(),
                other => panic!("expected Shell prompt, got {other:?}"),
            }
        }
    }
    result
}
fn notice(socket: &mut Socket) -> Value {
    loop {
        match socket.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Binary(_) => control(socket, json!({"type":"ack"})),
            Message::Ping(_) => socket.flush().unwrap(),
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
fn input(socket: &mut Socket, text: &str) {
    socket
        .send(Message::Binary(text.as_bytes().to_vec().into()))
        .unwrap();
}
fn output_until(socket: &mut Socket, expected: &str) -> String {
    let mut output = Vec::new();
    loop {
        match socket.read().unwrap() {
            Message::Binary(bytes) => {
                output.extend(bytes);
                control(socket, json!({"type":"ack"}));
            }
            Message::Text(text) => assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap()["type"],
                "input_ack"
            ),
            Message::Ping(_) => socket.flush().unwrap(),
            other => panic!("unexpected output: {other:?}"),
        }
        let text = String::from_utf8_lossy(&output);
        if text.contains(expected) {
            return text.into_owned();
        }
    }
}
fn close(socket: &mut Socket) {
    control(socket, json!({"type":"close"}));
    loop {
        let message = notice(socket);
        if message["type"] == "closed" {
            break;
        }
        assert_ne!(message["type"], "error", "{message}");
    }
}
#[track_caller]
fn wait_for(check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !check() {
        assert!(Instant::now() < deadline, "condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
}
#[track_caller]
fn pid(path: &Path) -> i32 {
    wait_for(|| fs::read_to_string(path).is_ok_and(|s| s.trim().parse::<i32>().is_ok()));
    fs::read_to_string(path).unwrap().trim().parse().unwrap()
}
fn gone(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) == Err(nix::errno::Errno::ESRCH)
}

#[test]
fn terminal_bytes_resize_disconnect_and_source_removal_are_connection_owned() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let host = HostProcess::start(home.path());
    let mut api = host.connect();
    let registered = api.runtime("register_workspace",json!({"label":"M4 terminal", "primary_directory":workspace.path(), "additional_directories":[]}));
    let id = registered["workspace"]["workspace_id"].clone();
    let source = json!({"type":"workspace", "workspace_id":id});
    let mut first = socket(&host, None);
    let mut second = socket(&host, None);
    assert_eq!(
        open(&mut first, Some(host.access_token()), &source)["type"],
        "created"
    );
    assert_eq!(
        open(&mut second, Some(host.access_token()), &source)["type"],
        "created"
    );
    input(
        &mut first,
        "printf '\\033[31m中文\\033[0m\\n'; echo $$ > shell.pid; /bin/sh -c 'echo $$ > child.pid; sleep 60'\n",
    );
    output_until(&mut first, "\x1b[31m中文\x1b[0m");
    let shell = pid(&workspace.path().join("shell.pid"));
    let child = pid(&workspace.path().join("child.pid"));
    close(&mut first);
    wait_for(|| gone(shell) && gone(child));
    control(
        &mut second,
        json!({"type":"resize", "size":{"cols":95,"rows":35}}),
    );
    input(&mut second, "stty size\n");
    output_until(&mut second, "35 95");
    input(&mut second, "echo $$ > second.pid; yes M4_BACKPRESSURE\n");
    let second_pid = pid(&workspace.path().join("second.pid"));
    // 故意不消费、不 ACK 输出；删除来源仍须回收该 socket 的 PTY。
    api.runtime("remove_workspace", json!({"workspace_id":id}));
    wait_for(|| gone(second_pid));
    let mut removed = socket(&host, None);
    assert_eq!(
        open(&mut removed, Some(host.access_token()), &source)["type"],
        "error"
    );
    api.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
}

#[test]
fn cookie_and_first_frame_authentication_limits_logout_and_host_shutdown() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let host = HostProcess::start(home.path());
    let mut api = host.connect();
    let registered = api.runtime("register_workspace",json!({"label":"M4 auth", "primary_directory":workspace.path(), "additional_directories":[]}));
    let source =
        json!({"type":"workspace", "workspace_id":registered["workspace"]["workspace_id"]});
    let mut wrong = socket(&host, None);
    assert_eq!(
        open(&mut wrong, Some("invalid-token"), &source)["type"],
        "error"
    );
    let client = reqwest::blocking::Client::new();
    let login: Value = client
        .post(format!("{}/auth/login", host.base_url()))
        .bearer_auth(host.access_token())
        .json(&json!({"method":"desktop"}))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    let token = login["token"].as_str().unwrap();
    let port = reqwest::Url::parse(host.base_url())
        .unwrap()
        .port()
        .unwrap();
    let cookie = format!("ez_host_session_http_{port}={token}");
    let mut web = socket(&host, Some(&cookie));
    assert_eq!(open(&mut web, None, &source)["type"], "created");
    input(&mut web, "echo $$ > web.pid; sleep 60\n");
    let web_pid = pid(&workspace.path().join("web.pid"));
    client
        .post(format!("{}/auth/logout", host.base_url()))
        .bearer_auth(token)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    wait_for(|| gone(web_pid));
    let mut native = Vec::new();
    for _ in 0..8 {
        let mut next = socket(&host, None);
        assert_eq!(
            open(&mut next, Some(host.access_token()), &source)["type"],
            "created"
        );
        native.push(next);
    }
    let mut excess = socket(&host, None);
    let error = open(&mut excess, Some(host.access_token()), &source);
    assert_eq!(error["type"], "error");
    assert!(error["message"].as_str().unwrap().contains('8'));
    // 三个独立普通登录各开八个，再加原生八个，合计触及 Host 上限。
    for _ in 0..3 {
        let login: Value = client
            .post(format!("{}/auth/login", host.base_url()))
            .bearer_auth(host.access_token())
            .json(&json!({"method":"desktop"}))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        for _ in 0..8 {
            let mut next = socket(&host, None);
            assert_eq!(
                open(&mut next, login["token"].as_str(), &source)["type"],
                "created"
            );
            native.push(next);
        }
    }
    let mut full = socket(&host, None);
    let error = open(&mut full, Some(host.access_token()), &source);
    assert!(error["message"].as_str().unwrap().contains("32"));
    input(&mut native[0], "echo $$ > shutdown.pid; sleep 60\n");
    let shutdown_pid = pid(&workspace.path().join("shutdown.pid"));
    api.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
    wait_for(|| gone(shutdown_pid));
}

#[test]
fn missing_authentication_and_silent_peer_are_bounded() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let host = HostProcess::start(home.path());
    let mut api = host.connect();
    let registered = api.runtime("register_workspace",json!({"label":"M4 timeout", "primary_directory":workspace.path(), "additional_directories":[]}));
    let source =
        json!({"type":"workspace", "workspace_id":registered["workspace"]["workspace_id"]});
    let mut pending = Vec::new();
    for _ in 0..32 {
        pending.push(socket(&host, None));
    }
    let mut request = format!(
        "{}/user-terminals/socket",
        host.base_url().replace("http:", "ws:")
    )
    .into_client_request()
    .unwrap();
    request
        .headers_mut()
        .insert("Origin", "tauri://localhost".parse().unwrap());
    assert!(
        matches!(tungstenite::connect(request), Err(tungstenite::Error::Http(response)) if response.status().as_u16() == 429)
    );
    assert_eq!(notice(&mut pending[0])["type"], "error"); // 5 秒首帧 deadline。
    drop(pending);
    let mut quiet = socket(&host, None);
    assert_eq!(
        open(&mut quiet, Some(host.access_token()), &source)["type"],
        "created"
    );
    input(&mut quiet, "echo $$ > silent.pid; sleep 60\n");
    let pid = pid(&workspace.path().join("silent.pid"));
    // 客户端故意不 read，因此不回应 Ping；不依赖 unload/Close 帧回收。
    let deadline = Instant::now() + Duration::from_secs(34);
    while !gone(pid) {
        assert!(
            Instant::now() < deadline,
            "silent terminal outlived heartbeat budget"
        );
        thread::sleep(Duration::from_millis(50));
    }
    api.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
}

#[test]
fn session_deletion_reclaims_its_terminal_and_tcp_loss_does_not_cancel_a_run() {
    let home = tempfile::tempdir().unwrap();
    let provider = support::FakeProvider::start();
    support::write_config(home.path(), provider.endpoint(), "isolated-terminal-model");
    let markers = tempfile::tempdir().unwrap();
    let host = HostProcess::start(home.path());
    let mut api = host.connect();
    let created = api.runtime(
        "create_session",
        json!({"title":"M4 session", "model_selection":null}),
    );
    let id = created["session"]["session_id"].as_str().unwrap();
    let source = json!({"type":"session", "session_id":id, "locator":{"root":{"type":"session_private"}, "relative_path":""}});
    let mut terminal = socket(&host, None);
    assert_eq!(
        open(&mut terminal, Some(host.access_token()), &source)["type"],
        "created"
    );
    let marker = markers.path().join("session.pid");
    input(
        &mut terminal,
        &format!("echo $$ > '{}'; sleep 60\n", marker.display()),
    );
    let shell_pid = pid(&marker);
    let submitted = api.runtime("submit_input",json!({"session_id":id,"message":"BLOCK_FOR_RESTART","variant":"build","idempotency_key":"m4-run"}));
    let run_id = submitted["run"]["run_id"].as_str().unwrap();
    api.wait_for_status("running alongside PTY", id, run_id, &["running"]);
    drop(terminal); // TCP 断开，没有 close 控制帧或页面通知。
    wait_for(|| gone(shell_pid));
    assert_eq!(
        api.wait_for_status("PTY close leaves Run alive", id, run_id, &["running"])["status"],
        "running"
    );
    api.wait_for_status("Run completes independently", id, run_id, &["completed"]);

    let mut terminal = socket(&host, None);
    assert_eq!(
        open(&mut terminal, Some(host.access_token()), &source)["type"],
        "created"
    );
    let marker = markers.path().join("deleted-session.pid");
    input(
        &mut terminal,
        &format!("echo $$ > '{}'; sleep 60\n", marker.display()),
    );
    let shell_pid = pid(&marker);
    let preview = api.runtime("prepare_delete_session", json!({"session_id":id}));
    api.runtime(
        "delete_session",
        json!({"session_id":id,"confirmation_token":preview["confirmation_token"]}),
    );
    wait_for(|| gone(shell_pid));
    let mut deleted = socket(&host, None);
    assert_eq!(
        open(&mut deleted, Some(host.access_token()), &source)["type"],
        "error"
    );
    api.runtime("shutdown_runtime", json!({}));
    assert!(host.wait().status.success());
}
