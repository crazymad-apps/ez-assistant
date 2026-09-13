use super::*;

async fn spawn(script: &str) -> (Arc<TerminalProcess>, mpsc::UnboundedReceiver<TerminalEvent>) {
    let mut command = CommandBuilder::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-Command", script]);
    command.env("EZ_ASSISTANT_TEST_TOKEN", "must-not-reach-shell");
    spawn_builder(command).await
}

async fn spawn_builder(
    command: CommandBuilder,
) -> (Arc<TerminalProcess>, mpsc::UnboundedReceiver<TerminalEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let process = TerminalProcess::spawn_command(
        std::env::temp_dir(),
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        command,
        move |event| tx.send(event).map_err(|_| failure("test receiver closed")),
    )
    .await
    .expect("spawn ConPTY");
    (process, rx)
}

#[tokio::test]
async fn installed_interactive_shells_accept_input_and_exit() {
    use assistant_protocol::ShellKind;
    let mut tested = 0;
    for kind in [
        ShellKind::WindowsPowershell51,
        ShellKind::Cmd,
        ShellKind::Powershell7,
        ShellKind::GitBash,
    ] {
        let command = match crate::user_terminal::shell::interactive(Some(kind)) {
            Ok(command) => command,
            Err(_) if matches!(kind, ShellKind::Powershell7 | ShellKind::GitBash) => {
                eprintln!("{kind:?}: not installed");
                continue;
            }
            Err(error) => panic!("required shell unavailable: {error}"),
        };
        let (process, mut events) = spawn_builder(command).await;
        // 首次输出后模拟终端输入；ConPTY 光标查询由 next 负责回复。
        if let TerminalEvent::Output { .. } = next(&process, &mut events).await {
            process.acknowledge();
        }
        let input: &[u8] = match kind {
            ShellKind::Cmd => b"echo verified-shell\r\nexit 0\r\n",
            ShellKind::GitBash => b"printf 'verified-shell\\n'; exit 0\n",
            _ => b"Write-Output ('verified-' + 'shell'); exit 0\r\n",
        };
        process.write(input).unwrap();
        let mut output = Vec::new();
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match next(&process, &mut events).await {
                    TerminalEvent::Output { bytes } => {
                        output.extend(bytes);
                        process.acknowledge();
                    }
                    TerminalEvent::Exited { code } => {
                        assert_eq!(code, 0, "{kind:?}");
                        break;
                    }
                    _ => panic!("{kind:?} failed"),
                }
            }
        })
        .await;
        process.close().await.unwrap();
        assert!(outcome.is_ok(), "{kind:?} did not exit after input");
        assert!(
            String::from_utf8_lossy(&output).contains("verified-shell"),
            "{kind:?}"
        );
        tested += 1;
    }
    assert!(tested >= 2);
    assert!(crate::user_terminal::shell::interactive(Some(ShellKind::PosixSh)).is_err());
}

async fn next(
    process: &TerminalProcess,
    events: &mut mpsc::UnboundedReceiver<TerminalEvent>,
) -> TerminalEvent {
    let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("bounded terminal output")
        .expect("terminal event");
    if let TerminalEvent::Output { bytes } = &event
        && bytes.windows(4).any(|w| w == b"\x1b[6n")
    {
        process.write(b"\x1b[1;1R").expect("cursor report");
    }
    event
}

#[tokio::test]
async fn natural_exit_drains_utf8_and_filters_private_environment() {
    let (process, mut events) = spawn("Start-Sleep -Milliseconds 200; Write-Output '中文'; if ($env:EZ_ASSISTANT_TEST_TOKEN) {exit 99}; exit 7").await;
    let mut output = Vec::new();
    loop {
        match next(&process, &mut events).await {
            TerminalEvent::Output { bytes } => {
                output.extend(bytes);
                process.acknowledge();
            }
            TerminalEvent::Exited { code } => {
                assert_eq!(code, 7);
                break;
            }
            _ => panic!("unexpected ConPTY error"),
        }
    }
    assert!(String::from_utf8(output).unwrap().contains("中文"));
    tokio::time::timeout(Duration::from_secs(5), process.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(process.job.active_processes().unwrap(), 0);
}

#[tokio::test]
async fn close_idle_reader_and_descendants_is_bounded_and_idempotent() {
    let (process, mut events) = spawn("$p = Start-Process powershell.exe -ArgumentList '-NoProfile -Command Start-Sleep -Seconds 20' -PassThru; Write-Output 'child-ready'; Start-Sleep -Seconds 20").await;
    let mut output = Vec::new();
    while !String::from_utf8_lossy(&output).contains("child-ready") {
        if let TerminalEvent::Output { bytes } = next(&process, &mut events).await {
            output.extend(bytes);
            process.acknowledge();
        } else {
            panic!("terminal exited before child");
        }
    }
    assert!(process.job.active_processes().unwrap() >= 2);
    process
        .resize(PtySize {
            rows: 35,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), process.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(process.job.active_processes().unwrap(), 0);
    process.close().await.unwrap();
}

#[tokio::test]
async fn closing_without_ack_unblocks_output_backpressure() {
    let (process, mut events) = spawn("Start-Sleep -Milliseconds 200; 1..100000 | ForEach-Object { Write-Output ('output-' + $_) }").await;
    let mut ready = false;
    while !ready {
        if let TerminalEvent::Output { bytes } = next(&process, &mut events).await {
            ready = String::from_utf8_lossy(&bytes).contains("output-");
            if !ready {
                process.acknowledge();
            }
        } else {
            panic!("terminal exited before output");
        }
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(60), events.recv())
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(5), process.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(process.job.active_processes().unwrap(), 0);
}

#[tokio::test]
async fn ctrl_c_interrupts_a_non_reading_foreground_command() {
    use assistant_protocol::ShellKind;
    let mut tested = 0;
    for kind in [
        ShellKind::WindowsPowershell51,
        ShellKind::Cmd,
        ShellKind::Powershell7,
        ShellKind::GitBash,
    ] {
        let command = match crate::user_terminal::shell::interactive(Some(kind)) {
            Ok(command) => command,
            Err(_) if matches!(kind, ShellKind::Powershell7 | ShellKind::GitBash) => {
                eprintln!("{kind:?}: not installed; Ctrl+C not tested");
                continue;
            }
            Err(error) => panic!("required shell unavailable: {error}"),
        };
        verify_ctrl_c(kind, command).await;
        tested += 1;
        eprintln!("{kind:?}: Ctrl+C restored prompt and accepted the next command");
    }
    assert!(tested >= 2);
}

async fn verify_ctrl_c(kind: assistant_protocol::ShellKind, command: CommandBuilder) {
    use assistant_protocol::ShellKind;
    let (blocking, following): (&[u8], &[u8]) = match kind {
        ShellKind::WindowsPowershell51 | ShellKind::Powershell7 => (
            b"function prompt { 'CTRL-' + 'READY> ' }; Write-Output ('before-' + 'sleep'); Start-Sleep -Seconds 15; Write-Output ('missed-' + 'interrupt')\r\n",
            b"Write-Output ('after-' + 'interrupt')\r\n",
        ),
        // CMD 使用系统 ping 提供约15秒前台等待；提示符恢复期限远早于自然结束。
        ShellKind::Cmd => (
            b"prompt CTRL-REA^DY$G$S & ping -n 16 127.0.0.1\r\n",
            b"echo after-^interrupt\r\n",
        ),
        ShellKind::GitBash => (
            b"PS1='CTRL-'\"READY> \"; printf 'before-%s\\n' sleep; sleep 15; printf 'missed-%s\\n' interrupt\n",
            b"printf 'after-%s\\n' interrupt\n",
        ),
        ShellKind::PosixSh => unreachable!("Windows terminal matrix"),
    };
    let (process, mut events) = spawn_builder(command).await;
    if let TerminalEvent::Output { .. } = next(&process, &mut events).await {
        process.acknowledge();
    }
    let idle_process_count = process.job.active_processes().unwrap();
    process.write(blocking).unwrap();
    let mut output = Vec::new();
    // CMD 必须等 ping 的真实响应，避免在子进程尚未接管控制台前发出中断。
    let started_marker = if kind == ShellKind::Cmd {
        "TTL="
    } else {
        "before-sleep"
    };
    while !String::from_utf8_lossy(&output).contains(started_marker) {
        match next(&process, &mut events).await {
            TerminalEvent::Output { bytes } => {
                output.extend(bytes);
                process.acknowledge();
            }
            _ => panic!("terminal exited before interrupt"),
        }
    }
    if kind == ShellKind::GitBash {
        // bash 的 before marker 先于 fork/exec；等待 Job 中出现 sleep 子进程。
        let started = tokio::time::timeout(Duration::from_secs(5), async {
            while process.job.active_processes().unwrap() <= idle_process_count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if started.is_err() {
            process.close().await.unwrap();
            panic!("Git Bash foreground child did not start");
        }
    }
    process.write(b"\x03").unwrap();
    // Ctrl+C 会取消当前控制台输入；等真实提示符恢复后再输入，避免将清空输入误判为未中断。
    // 提示符和输出 marker 都由字符串拼接产生，命令回显不能满足断言。
    let interrupted = tokio::time::timeout(Duration::from_secs(5), async {
        let mut ready_output = Vec::new();
        loop {
            match next(&process, &mut events).await {
                TerminalEvent::Output { bytes } => {
                    ready_output.extend_from_slice(&bytes);
                    output.extend(bytes);
                    process.acknowledge();
                    // ConPTY 可将尾部空格编码为光标移动；只匹配提示符的可见字符。
                    if String::from_utf8_lossy(&ready_output).contains("CTRL-READY>") {
                        break;
                    }
                }
                _ => panic!("Ctrl+C must restore the interactive shell prompt"),
            }
        }
        process.write(following).unwrap();
        loop {
            match next(&process, &mut events).await {
                TerminalEvent::Output { bytes } => {
                    output.extend(bytes);
                    process.acknowledge();
                    if String::from_utf8_lossy(&output).contains("after-interrupt") {
                        break;
                    }
                }
                TerminalEvent::Exited { .. } => {
                    panic!("Ctrl+C must preserve the interactive shell")
                }
                _ => panic!("terminal failed during interrupt"),
            }
        }
    })
    .await;
    process.close().await.unwrap();
    assert!(
        interrupted.is_ok(),
        "{kind:?}: Ctrl+C did not restore a usable prompt: {:?}",
        String::from_utf8_lossy(&output)
    );
    if kind != ShellKind::Cmd {
        assert!(!String::from_utf8_lossy(&output).contains("missed-interrupt"));
    }
    assert_eq!(process.job.active_processes().unwrap(), 0);
}
