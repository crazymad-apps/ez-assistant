use std::{num::NonZeroU64, sync::Arc, time::Duration};

use agent_tools::{AbsolutePath, ShellProcessMode, ShellRequest, ShellTool};
use agent_tools_local::{EnvironmentPolicy, LocalShell, LocalShellConfig};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::*;

#[tokio::test]
async fn installed_launchers_preserve_unicode_quotes_workdir_and_exit_status() {
    let root = TempDir::new().expect("temporary root");
    let directory = root.path().join("中文 space & quoted");
    std::fs::create_dir(&directory).expect("working directory");
    std::fs::write(directory.join("quoted file.txt"), "file 中文").expect("fixture");
    // 探测会读取本机注册表，放在阻塞池中；每个可用解释器必须真实执行。
    let entries = tokio::task::spawn_blocking(catalog)
        .await
        .expect("catalog task");
    let mut executed = Vec::new();
    for entry in entries {
        if !entry.available {
            assert!(entry.reason.is_some());
            continue;
        }
        let snapshot = tokio::task::spawn_blocking(move || freeze(Some(entry.kind)))
            .await
            .expect("freeze task")
            .expect("available shell");
        let command = match snapshot.kind {
            ShellKind::WindowsPowershell51 | ShellKind::Powershell7 => {
                "Write-Output 'hello 中文 \"quoted\"';\n[Console]::WriteLine([IO.File]::ReadAllText((Join-Path (Get-Location) 'quoted file.txt'))); [Console]::Error.WriteLine('error 中文'); exit 7"
            }
            ShellKind::Cmd => {
                "echo hello 中文 \"quoted\" & type \"quoted file.txt\" & echo error 中文 1>&2 & exit /b 7"
            }
            ShellKind::GitBash | ShellKind::PosixSh => {
                "printf '%s\\n' 'hello 中文 \"quoted\"'\ncat 'quoted file.txt'\nprintf '%s' 'error 中文' >&2\nexit 7"
            }
        };
        let shell = LocalShell::new(LocalShellConfig {
            launcher: launcher(&snapshot),
            environment: EnvironmentPolicy::default(),
        });
        let outcome = shell
            .exec(
                ShellRequest {
                    command: command.to_owned(),
                    workdir: AbsolutePath::new(&directory).expect("absolute directory"),
                    timeout: Duration::from_secs(10),
                    max_output_bytes: NonZeroU64::new(4096).expect("output limit"),
                    process_mode: ShellProcessMode::Managed,
                },
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .expect("execute shell");
        assert_eq!(
            outcome.exit_code,
            Some(7),
            "{:?}: {outcome:?}",
            snapshot.kind
        );
        assert!(
            outcome.stdout.contains("hello 中文 \"quoted\""),
            "{:?}: {outcome:?}",
            snapshot.kind
        );
        assert!(
            outcome.stdout.contains("file 中文"),
            "{:?}: {outcome:?}",
            snapshot.kind
        );
        assert!(
            outcome.stderr.contains("error 中文"),
            "{:?}: {outcome:?}",
            snapshot.kind
        );
        executed.push(snapshot.kind);
    }
    let default = freeze(None).expect("platform default");
    assert!(
        executed.contains(&default.kind),
        "default launcher must execute"
    );
    eprintln!("executed launchers: {executed:?}");
}
