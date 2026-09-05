use super::{TerminalEvent, TerminalSize, process::TerminalProcess};
use portable_pty::CommandBuilder;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

async fn spawn(script: &str) -> (Arc<TerminalProcess>, mpsc::UnboundedReceiver<TerminalEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", script]);
    command.env("EZ_ASSISTANT_TEST_TOKEN", "must-not-reach-shell");
    let process = TerminalProcess::spawn_command(
        std::env::temp_dir(),
        TerminalSize { cols: 80, rows: 24 }
            .validate()
            .expect("size"),
        command,
        move |event| {
            tx.send(event)
                .map_err(|_| super::failure("test receiver closed"))
        },
    )
    .await
    .expect("spawn PTY");
    (process, rx)
}

#[tokio::test]
async fn output_requires_ack_and_close_unblocks_backpressure() {
    let (process, mut events) = spawn("yes terminal-output").await;
    assert!(matches!(
        events.recv().await,
        Some(TerminalEvent::Output { .. })
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(60), events.recv())
            .await
            .is_err()
    );
    process.acknowledge();
    assert!(matches!(
        events.recv().await,
        Some(TerminalEvent::Output { .. })
    ));
    tokio::time::timeout(Duration::from_secs(3), process.close())
        .await
        .expect("bounded close")
        .expect("close");
    process.close().await.expect("idempotent close");
}

#[tokio::test]
async fn raw_output_preserves_utf8_ansi_and_exit_after_last_ack() {
    let (process, mut events) = spawn(r"printf '\033[31m中文\033[0m'; exit 7").await;
    let mut output = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(3), events.recv())
            .await
            .expect("event")
        {
            Some(TerminalEvent::Output { bytes }) => {
                output.extend(bytes);
                process.acknowledge();
            }
            Some(TerminalEvent::Exited { code }) => {
                assert_eq!(code, 7);
                break;
            }
            _ => panic!("unexpected terminal event"),
        }
    }
    assert_eq!(output, "\x1b[31m中文\x1b[0m".as_bytes());
    process.close().await.expect("close exited terminal");
}

#[tokio::test]
async fn shell_environment_omits_private_credentials() {
    let (process, mut events) = spawn("printf '%s' \"${EZ_ASSISTANT_TEST_TOKEN:-removed}\"").await;
    let mut output = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .expect("event")
    {
        match event {
            TerminalEvent::Output { bytes } => {
                output.extend(bytes);
                process.acknowledge();
            }
            TerminalEvent::Exited { .. } => break,
            TerminalEvent::Error { message } => panic!("{message}"),
        }
    }
    assert_eq!(output, b"removed");
    process.close().await.expect("close");
}

#[tokio::test]
async fn closing_idle_terminal_cancels_nonblocking_reader() {
    let (process, _events) = spawn("sleep 60").await;
    tokio::time::timeout(Duration::from_secs(3), process.close())
        .await
        .expect("close must not wait for output")
        .expect("close");
}

#[tokio::test]
async fn invalid_shell_does_not_create_a_terminal() {
    let result = TerminalProcess::spawn_command(
        std::env::temp_dir(),
        TerminalSize { cols: 80, rows: 24 }
            .validate()
            .expect("size"),
        CommandBuilder::new("/nonexistent/ez-test-shell"),
        |_| Ok(()),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn closing_does_not_submit_unfinished_input() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("must-not-execute");
    let (process, mut events) = spawn("PS1=ready; exec /bin/sh -i").await;
    if let Some(TerminalEvent::Output { .. }) = events.recv().await {
        process.acknowledge();
    }
    process
        .write(format!("touch '{}'", marker.display()).as_bytes())
        .expect("input without Enter");
    process.close().await.expect("close pending input");
    assert!(
        !marker.exists(),
        "closing must not execute the incomplete command"
    );
}

#[tokio::test]
async fn reload_cleanup_cancels_all_unacknowledged_terminals() {
    let manager = super::UserTerminalManager::default();
    let (first, mut first_events) = spawn("yes first").await;
    let (second, mut second_events) = spawn("yes second").await;
    first_events.recv().await.expect("first output");
    second_events.recv().await.expect("second output");
    manager.terminals.lock().await.insert("first".into(), first);
    manager
        .terminals
        .lock()
        .await
        .insert("second".into(), second);
    manager
        .close_all()
        .await
        .expect("all PTYs reclaimed despite missing ack");
    assert!(manager.terminals.lock().await.is_empty());
}

#[tokio::test]
async fn shutdown_waits_for_creation_then_blocks_new_terminals_until_resumed() {
    let manager = Arc::new(super::UserTerminalManager::default());
    let creation_gate = manager.lifecycle.lock().await;
    let (process, mut events) = spawn("yes shutdown-pending").await;
    events.recv().await.expect("unacknowledged output");
    let owner = manager.clone();
    let shutdown = tokio::spawn(async move { owner.shutdown().await });
    manager
        .terminals
        .lock()
        .await
        .insert("creating".into(), process.clone());
    drop(creation_gate);
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .expect("bounded shutdown")
        .expect("shutdown task")
        .expect("PTY reclaimed");
    assert!(manager.terminals.lock().await.is_empty());
    assert!(super::ensure_accepting(*manager.lifecycle.lock().await).is_err());
    manager
        .close_all()
        .await
        .expect("reload cannot reopen a shutdown owner");
    assert!(super::ensure_accepting(*manager.lifecycle.lock().await).is_err());
    manager.resume().await;
    assert!(super::ensure_accepting(*manager.lifecycle.lock().await).is_ok());
    process.close().await.expect("idempotent cleanup");
}
