use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

use agent_tools::{AbsolutePath, BackgroundOutputRequest};
use tempfile::tempdir;
use tokio::time::timeout;

use super::*;
use crate::{EnvironmentPolicy, LocalShellConfig};

const ID: &str = "background_abcdefghijkl";
const OTHER_ID: &str = "background_abcdefghijkm";

fn adapter(home: &Path) -> LocalBackgroundShell {
    LocalBackgroundShell::new(
        Arc::new(LocalShell::new(LocalShellConfig::new(
            EnvironmentPolicy::default(),
        ))),
        home.to_owned(),
    )
}

fn request(home: &Path, command: &str) -> BackgroundShellRequest {
    BackgroundShellRequest {
        command: command.into(),
        workdir: AbsolutePath::new(home.to_owned()).unwrap(),
    }
}

fn page(cursor: Option<u64>, offset_chars: i64, limit_chars: usize) -> BackgroundOutputRequest {
    BackgroundOutputRequest {
        cursor,
        offset_chars,
        limit_chars,
    }
}

#[tokio::test]
async fn rolling_output_is_utf8_bounded_and_retains_absolute_positions() {
    let home = tempdir().unwrap();
    let path = home.path().join("output.log");
    let log = Arc::new(Log::open(path.clone()).await.unwrap());
    let (tx, rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
    let writer = {
        let log = log.clone();
        tokio::spawn(async move { log.collect(rx).await })
    };
    let suffix = "汉🙂".repeat(1024);
    let chunk = |index| format!("{index:05}{suffix}");
    let chunk_chars = 2 * 1024 + 5;
    let chunk_count = 135 * 1024 * 1024 / chunk(0).len();
    for index in 0..chunk_count {
        tx.send(super::super::output::RawOutput {
            channel: ShellOutputChannel::Stdout,
            bytes: chunk(index).into_bytes(),
        })
        .await
        .unwrap();
    }
    for (channel, bytes) in [
        (ShellOutputChannel::Stdout, &"终🙂".as_bytes()[..5]),
        (ShellOutputChannel::Stderr, &b"!"[..]),
        (ShellOutputChannel::Stdout, &"终🙂".as_bytes()[5..]),
    ] {
        tx.send(super::super::output::RawOutput {
            channel,
            bytes: bytes.to_vec(),
        })
        .await
        .unwrap();
    }
    drop(tx);
    timeout(Duration::from_secs(10), writer)
        .await
        .unwrap()
        .unwrap();
    let latest = log.read(page(None, 0, 3)).await.unwrap();
    assert_eq!(latest.content, "终!🙂");
    assert_eq!(latest.next_cursor, (chunk_count * chunk_chars + 3) as u64);
    assert_eq!(latest.dropped_chars, 0);
    let old = log.read(page(Some(0), 0, 8)).await.unwrap();
    assert!(old.cursor > 0);
    assert_eq!(old.cursor, old.dropped_chars);
    assert_eq!(old.cursor % chunk_chars as u64, 0);
    assert_eq!(
        old.content,
        format!("{:05}汉🙂汉", old.cursor / chunk_chars as u64)
    );
    let middle = (chunk_count - 200) / 10 * 10;
    let requested = (middle * chunk_chars) as u64;
    let across_checkpoints = log.read(page(Some(requested), -2, 8_000)).await.unwrap();
    let expected: String = (middle - 1..middle + 4)
        .flat_map(|index| chunk(index).chars().collect::<Vec<_>>())
        .skip(chunk_chars - 2)
        .take(8_000)
        .collect();
    assert_eq!(across_checkpoints.content, expected);
    assert_eq!(across_checkpoints.cursor, requested - 2);
    assert_eq!(across_checkpoints.next_cursor, requested - 2 + 8_000);
    assert_eq!(
        log.read(page(Some(latest.next_cursor), -3, 3))
            .await
            .unwrap(),
        latest
    );
    assert!(
        log.read(page(Some(latest.next_cursor), 0, 3))
            .await
            .unwrap()
            .content
            .is_empty()
    );
    assert!(
        log.read(page(Some(latest.next_cursor), 1, 3))
            .await
            .is_err()
    );
    {
        let mut saved = fs::File::open(path).unwrap();
        assert!((75 * 1024 * 1024..=100 * 1024 * 1024).contains(&saved.metadata().unwrap().len()));
        saved
            .seek(SeekFrom::End(-(latest.content.len() as i64)))
            .unwrap();
        let mut tail = Vec::new();
        saved.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, latest.content.as_bytes());
    }
    // 翻回中部后继续写入，追加位置不受磁盘读取位置影响。
    log.read(page(Some(requested), 0, 10)).await.unwrap();
    let (tx, rx) = mpsc::channel(1);
    tx.send(super::super::output::RawOutput {
        channel: ShellOutputChannel::Stdout,
        bytes: b"next".to_vec(),
    })
    .await
    .unwrap();
    drop(tx);
    log.collect(rx).await;
    assert_eq!(
        log.read(page(Some(latest.next_cursor), 0, 4))
            .await
            .unwrap()
            .content,
        "next"
    );
    log.close().await.unwrap();
    assert!(log.read(page(None, 0, 3)).await.is_err());
}

#[tokio::test]
async fn natural_exit_returns_actual_code_and_removes_temporary_files() {
    let home = tempdir().unwrap();
    #[cfg(unix)]
    let script = "printf example; exit 7";
    #[cfg(windows)]
    let script = "<nul set /p =example& exit /b 7";
    let mut process = adapter(home.path())
        .start(ID, request(home.path(), script))
        .await
        .unwrap();
    let output = process.output();
    let result = timeout(Duration::from_secs(5), async {
        process.wait_parent().await.unwrap();
        process.finish().await.unwrap()
    })
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.output.unwrap().content, "example");
    assert!(result.output_error.is_none());
    assert!(!output.path().exists());
}

#[tokio::test]
async fn old_instance_recovery_stops_only_the_matching_pid() {
    let root = tempdir().unwrap();
    let users = root.path().join("users");
    let home = users.join("_personal");
    fs::create_dir_all(&home).unwrap();
    let shell = adapter(&home);
    #[cfg(unix)]
    let script = "exec sleep 30";
    #[cfg(windows)]
    let script = "ping -n 31 127.0.0.1 >NUL";
    let mut matched = shell.start(ID, request(&home, script)).await.unwrap();
    let mut different = shell.start(OTHER_ID, request(&home, script)).await.unwrap();
    let matched_log = matched.output().path().to_owned();
    let different_log = different.output().path().to_owned();
    fs::rename(
        matched_log.with_file_name("identity.json"),
        matched_log.with_file_name("identity.tmp"),
    )
    .unwrap();
    let identity_path = different_log.with_file_name("identity.json");
    let mut wrong: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    wrong["created"] = (wrong["created"].as_u64().unwrap() + 1).into();
    fs::write(&identity_path, serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert!(recover_background_shells(&users).unwrap().is_empty());
    timeout(Duration::from_secs(3), matched.wait_parent())
        .await
        .unwrap()
        .unwrap();
    assert!(
        timeout(Duration::from_millis(100), different.wait_parent())
            .await
            .is_err()
    );
    assert!(!matched_log.exists());
    assert!(!different_log.exists());
    timeout(Duration::from_secs(5), different.finish())
        .await
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn broken_identity_is_preserved_and_symlink_is_never_read() {
    let root = tempdir().unwrap();
    let users = root.path().join("users");
    let home = users.join("_personal");
    fs::create_dir_all(&home).unwrap();
    let path = recovery::create_run_directory(&home.join(BACKGROUND_SHELL_DIRECTORY), ID).unwrap();
    let outside = root.path().join("unrelated.json");
    fs::write(&outside, "unrelated").unwrap();
    std::os::unix::fs::symlink(&outside, path.join("identity.json")).unwrap();
    fs::write(path.join(recovery::OUTPUT_FILE), "keep").unwrap();
    assert_eq!(recover_background_shells(&users).unwrap().len(), 1);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "unrelated");
    assert_eq!(
        fs::read_to_string(path.join(recovery::OUTPUT_FILE)).unwrap(),
        "keep"
    );
    fs::remove_file(path.join("identity.json")).unwrap();
    fs::write(path.join("identity.json"), "{partial").unwrap();
    assert_eq!(recover_background_shells(&users).unwrap().len(), 1);
    assert!(path.join("identity.json").exists());
}

#[tokio::test]
async fn failed_launch_releases_its_temporary_files() {
    let home = tempdir().unwrap();
    let mut config = LocalShellConfig::new(EnvironmentPolicy::default());
    config.launcher.program = home.path().join("missing-interpreter").into_os_string();
    let shell =
        LocalBackgroundShell::new(Arc::new(LocalShell::new(config)), home.path().to_owned());
    assert!(
        shell
            .start(ID, request(home.path(), "echo check"))
            .await
            .is_err()
    );
    assert!(
        !home
            .path()
            .join(BACKGROUND_SHELL_DIRECTORY)
            .join(ID)
            .exists()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn natural_parent_exit_terminates_its_redirected_descendant() {
    let home = tempdir().unwrap();
    let script = "sleep 30 >/dev/null 2>&1 & printf '%s' \"$!\"";
    let mut process = adapter(home.path())
        .start(ID, request(home.path(), script))
        .await
        .unwrap();
    let result = timeout(Duration::from_secs(5), async {
        process.wait_parent().await.unwrap();
        process.finish().await.unwrap()
    })
    .await
    .unwrap();
    let pid = result.output.unwrap().content.parse::<u32>().unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            if identity::capture(pid).unwrap().is_none() {
                break;
            }
            // Linux 中由其他父进程回收的僵尸不可继续执行，允许等待到 init reap。
            #[cfg(target_os = "linux")]
            if fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with("Z "))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn identity_write_failure_cleans_the_started_tree_before_returning() {
    let home = tempdir().unwrap();
    let shell = adapter(home.path());
    let directory = recovery::create_run_directory(&shell.root, ID).unwrap();
    fs::write(directory.join("identity.tmp"), "blocked staging").unwrap();
    let result = timeout(
        Duration::from_secs(5),
        shell.spawn_in(
            &directory,
            request(home.path(), "sleep 0.3; printf leaked > marker"),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ShellToolError::Io { .. })));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!home.path().join("marker").exists());
}
