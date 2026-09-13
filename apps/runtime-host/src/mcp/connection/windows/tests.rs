use std::{fs, process::Stdio, sync::Arc, time::Duration};

use super::*;
use crate::mcp::connection::{
    INHERITED_ENVIRONMENT, ManagedStdioTransport, McpProcessCleanup, serve, spawn_managed,
};
use tokio::io::AsyncBufReadExt as _;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[test]
fn resolves_explicit_paths_and_final_path_without_other_program_fallback() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("中文 first");
    let second = root.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(first.join("tool.cmd"), b"").unwrap();
    fs::write(second.join("tool.exe"), b"").unwrap();
    let search = std::env::join_paths([&first, &second]).unwrap();
    assert_eq!(
        resolve_path("tool", root.path(), Some(&search)),
        Some(second.join("tool.exe"))
    );
    assert_eq!(
        resolve_path("tool.cmd", root.path(), Some(&search)),
        Some(first.join("tool.cmd"))
    );
    let environment = BTreeMap::from([("Path".into(), first.to_str().unwrap().into())]);
    assert_eq!(
        resolve_program("tool", root.path(), &environment).unwrap(),
        first.join("tool.cmd")
    );
    assert_eq!(
        resolve_path(r"second\tool", root.path(), None),
        Some(second.join("tool.exe"))
    );
    assert!(resolve_path("missing", root.path(), Some(&search)).is_none());
    assert!(resolve_program("secret-command-never-echo", root.path(), &environment).is_err());
}

// 真实 stdio 协商经过与生产相同的解析、参数数组、Job 和 Transport；不联网、不写用户 npm cache。
#[tokio::test]
async fn windows_exe_cmd_bat_node_npx_python_negotiate_and_close() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("中文 workspace");
    fs::create_dir(&cwd).unwrap();
    let environment = BTreeMap::new();
    let python = resolve_program("python", &cwd, &environment).unwrap();
    let node = resolve_program("node", &cwd, &environment).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py");
    let script = cwd.join("bridge.cjs");
    fs::write(&script, format!(
        "const p=require('node:child_process').spawn({},[{}],{{stdio:'inherit'}});p.on('exit',c=>process.exit(c??1));",
        serde_json::to_string(&python).unwrap(), serde_json::to_string(&fixture).unwrap(),
    )).unwrap();
    for extension in ["cmd", "bat"] {
        fs::write(
            cwd.join(format!("bridge.{extension}")),
            format!(
                "@echo off\r\n\"{}\" \"{}\" %*\r\n",
                python.display(),
                fixture.display(),
            ),
        )
        .unwrap();
    }
    let cases = [
        (
            python.to_str().unwrap().to_owned(),
            vec![fixture.to_str().unwrap().to_owned()],
        ),
        (cwd.join("bridge.cmd").to_str().unwrap().to_owned(), vec![]),
        (cwd.join("bridge.bat").to_str().unwrap().to_owned(), vec![]),
        ("node".into(), vec![script.to_str().unwrap().to_owned()]),
        (
            "npx".into(),
            vec![
                "--offline".into(),
                "--no".into(),
                "--".into(),
                node.to_str().unwrap().into(),
                script.to_str().unwrap().into(),
            ],
        ),
        ("python".into(), vec![fixture.to_str().unwrap().to_owned()]),
    ];
    for (program, args) in cases {
        let program = resolve_program(&program, &cwd, &environment).unwrap();
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&cwd)
            .env_clear()
            .env("npm_config_cache", cwd.join("npm-cache"))
            .env("npm_config_update_notifier", "false")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in INHERITED_ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = spawn_managed(command).unwrap();
        let stdin = child.stdin().take().unwrap();
        let stdout = child.stdout().take().unwrap();
        let stderr = child.stderr().take().unwrap();
        let cleanup = Arc::new(McpProcessCleanup::default());
        let transport = ManagedStdioTransport::new(stdout, stdin, stderr, child, cleanup.clone());
        let mut service = tokio::time::timeout(
            Duration::from_secs(15),
            serve(transport, CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            service.list_tools(None).await.unwrap().tools[0].name,
            "first_tool"
        );
        service.close().await.unwrap();
        cleanup.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn batch_arguments_remain_data_and_do_not_execute_commands() {
    let root = tempfile::tempdir().unwrap();
    let python = resolve_program("python", root.path(), &BTreeMap::new()).unwrap();
    let script = root.path().join("args.py");
    fs::write(
        &script,
        "import json,sys\nprint(json.dumps(sys.argv[1:]))\n",
    )
    .unwrap();
    let batch = root.path().join("argument shim.cmd");
    fs::write(
        &batch,
        format!(
            "@echo off\r\n\"{}\" \"{}\" %*\r\n",
            python.display(),
            script.display()
        ),
    )
    .unwrap();
    let arguments = [
        "中文 space",
        "",
        "a&echo injected",
        "x|more",
        "(value)",
        "a^b",
        "100%",
        "hello!",
    ];
    let output = Command::new(batch).args(arguments).output().await.unwrap();
    assert!(output.status.success());
    let actual: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, arguments);
}

#[tokio::test]
async fn batch_descendants_are_reaped_on_close_drop_and_cancelled_handshake() {
    use rmcp::{RoleClient, transport::Transport};
    let root = tempfile::tempdir().unwrap();
    let node = resolve_program("node", root.path(), &BTreeMap::new()).unwrap();
    let descendant = root.path().join("descendant.cjs");
    fs::write(&descendant, "const s=require('node:net').createServer();s.listen(0,'127.0.0.1',()=>console.log(s.address().port));").unwrap();
    let parent = root.path().join("parent.cjs");
    fs::write(&parent, format!("require('node:child_process').spawn(process.execPath,[{}],{{stdio:'inherit'}});setInterval(()=>{{}},1000);", serde_json::to_string(&descendant).unwrap())).unwrap();
    let batch = root.path().join("parent.cmd");
    fs::write(
        &batch,
        format!(
            "@echo off\r\n\"{}\" \"{}\"\r\n",
            node.display(),
            parent.display()
        ),
    )
    .unwrap();
    for mode in ["close", "drop", "cancel"] {
        let mut command = Command::new(&batch);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spawn_managed(command).unwrap();
        let mut stdout = child.stdout().take().unwrap();
        let mut ready = String::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::io::BufReader::new(&mut stdout).read_line(&mut ready),
        )
        .await
        .unwrap()
        .unwrap();
        let address = format!("127.0.0.1:{}", ready.trim());
        assert!(tokio::net::TcpStream::connect(&address).await.is_ok());
        let stdin = child.stdin().take().unwrap();
        let stderr = child.stderr().take().unwrap();
        let cleanup = Arc::new(McpProcessCleanup::default());
        let mut transport =
            ManagedStdioTransport::new(stdout, stdin, stderr, child, cleanup.clone());
        if mode == "close" {
            <ManagedStdioTransport as Transport<RoleClient>>::close(&mut transport)
                .await
                .unwrap();
            drop(transport);
        } else if mode == "cancel" {
            let cancellation = CancellationToken::new();
            let negotiation = serve(transport, cancellation.clone());
            tokio::pin!(negotiation);
            assert!(futures_util::poll!(&mut negotiation).is_pending());
            cancellation.cancel();
            assert!(negotiation.await.is_err());
        } else {
            drop(transport);
        }
        cleanup.shutdown().await.unwrap();
        assert!(
            tokio::net::TcpStream::connect(&address).await.is_err(),
            "descendant still alive after {mode}"
        );
    }
}
