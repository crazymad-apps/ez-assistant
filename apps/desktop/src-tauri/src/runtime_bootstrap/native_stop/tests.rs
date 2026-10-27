use super::*;

#[tokio::test]
#[ignore = "需要 EZ_ASSISTANT_TEST_HOST_BINARY；Windows 另需新建并核实的 EARLY_HOME"]
async fn isolated_native_stop_before_discovery_and_http() {
    use super::super::{RuntimeBootstrapCoordinator, instance_lock_released};
    use std::{process::Stdio, time::Duration};
    let binary = PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_HOST_BINARY").unwrap())
        .canonicalize()
        .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.runtime-test")
        .canonicalize()
        .unwrap();
    #[cfg(unix)]
    let directory = tempfile::Builder::new()
        .prefix("f01-native-early-")
        .tempdir_in(&root)
        .unwrap();
    #[cfg(unix)]
    let home = {
        use std::os::unix::fs::PermissionsExt as _;
        fs::write(directory.path().join("host.toml"),"version = \"0.27.0\"\nmode = \"enterprise\"\n[enterprise]\ncenter_url = \"http://127.0.0.1:9\"\n").unwrap();
        fs::set_permissions(
            directory.path().join("host.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        directory.path().to_owned()
    };
    #[cfg(windows)]
    let home = {
        let path = PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_EARLY_HOME").unwrap())
            .canonicalize()
            .unwrap();
        assert!(path.starts_with(&root));
        assert_eq!(
            fs::read_to_string(path.join("f01-native-validation")).unwrap(),
            "v0.32.1"
        );
        path
    };
    assert!(!home.join(super::super::DISCOVERY_RELATIVE_PATH).exists());
    let mut child = tokio::process::Command::new(&binary)
        .arg("serve")
        .arg("--runtime-home")
        .arg(&home)
        .arg("--password-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let coordinator = RuntimeBootstrapCoordinator::new(home.clone(), binary.clone()).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !home.join("run/runtime.lock").exists() || instance_lock_released(&home).unwrap_or(true) {
        assert!(
            child.try_wait().unwrap().is_none(),
            "early Host exited before acquiring the lock"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "early Host did not acquire the lock"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!home.join(super::super::DISCOVERY_RELATIVE_PATH).exists());
    let previous = RuntimeDiscovery {
        address: "http://127.0.0.1:1".into(),
        instance_id: "previous-instance".into(),
        access_token: "a".repeat(43),
        pid: std::process::id(),
        executable_path: Some(binary.clone()),
        executable_sha256: None,
    };
    assert!(stop(&home, &binary, Some(&previous)).is_err());
    assert!(child.try_wait().unwrap().is_none());
    coordinator.shutdown().await.unwrap();
    assert!(!child.wait().await.unwrap().success());
    assert!(instance_lock_released(&home).unwrap());
    assert!(!home.join(super::super::DISCOVERY_RELATIVE_PATH).exists());
    assert!(!home.join("users").exists());
}

fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join("run")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(home.path().join("run"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    home
}

#[test]
fn stopping_absent_home_does_not_create_or_launch_anything() {
    let parent = tempfile::tempdir().unwrap();
    let home = parent.path().join("absent");
    assert_eq!(stop(&home, &home.join("absent-host"), None).unwrap(), None);
    assert!(!home.exists());
    assert!(stop(Path::new("relative-home"), Path::new("host"), None).is_err());
}

#[test]
fn missing_discovery_and_lock_do_not_prove_a_previously_confirmed_process_exited() {
    let parent = tempfile::tempdir().unwrap();
    let home = parent.path().join("absent");
    let expected = RuntimeDiscovery {
        address: "http://127.0.0.1:1".into(),
        instance_id: "confirmed".into(),
        access_token: "a".repeat(43),
        pid: std::process::id(),
        executable_path: None,
        executable_sha256: None,
    };
    assert!(stop(&home, &home.join("host"), Some(&expected)).is_err());
    assert!(!home.exists());
}

#[test]
fn lock_held_by_this_desktop_is_not_authority_to_kill_itself() {
    let home = home();
    let lock = home.path().join("run/runtime.lock");
    fs::write(&lock, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let held = File::options().read(true).write(true).open(&lock).unwrap();
    held.try_lock().unwrap();
    assert!(stop(home.path(), &std::env::current_exe().unwrap(), None).is_err());
    let other = File::options().read(true).write(true).open(&lock).unwrap();
    assert!(!available(&other).unwrap());
    held.unlock().unwrap();
    assert!(available(&other).unwrap());
}

#[test]
fn stale_discovery_cannot_stop_a_live_process_without_lock_ownership() {
    let home = home();
    let discovery = home.path().join(super::super::DISCOVERY_RELATIVE_PATH);
    let content = serde_json::to_vec(&serde_json::json!({
        "address": "http://127.0.0.1:1", "instance_id": "stale",
        "access_token": "a".repeat(43), "pid": std::process::id(),
        "executable_path": std::env::current_exe().unwrap(), "executable_sha256": "obsolete",
    }))
    .unwrap();
    fs::write(&discovery, &content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&discovery, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(stop(home.path(), &std::env::current_exe().unwrap(), None).is_err());
    assert_eq!(fs::read(discovery).unwrap(), content);
}

#[test]
fn replaced_lock_file_is_not_the_captured_resource() {
    let parent = tempfile::tempdir().unwrap();
    let a = File::create(parent.path().join("a")).unwrap();
    let same = File::open(parent.path().join("a")).unwrap();
    let b = File::create(parent.path().join("b")).unwrap();
    assert!(same_lock(&a, &same).unwrap());
    assert!(!same_lock(&a, &b).unwrap());
}

/// 不连接企业中心、不登录或初始化用户库；只创建本轮独占的临时 Home。
#[tokio::test]
#[ignore = "需要 EZ_ASSISTANT_TEST_HOST_BINARY；真实启动中 Host 的原生停止／当前来源重启"]
async fn isolated_native_restart_uses_current_bundle_without_old_http_or_digest() {
    use super::super::{RuntimeBootstrapCoordinator, instance_lock_released, read_discovery};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::{process::Stdio, time::Duration};
    let binary = PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_HOST_BINARY").unwrap())
        .canonicalize()
        .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.runtime-test")
        .canonicalize()
        .unwrap();
    let directory = tempfile::Builder::new()
        .prefix("f01-native-restart-")
        .tempdir_in(root)
        .unwrap();
    let old = directory.path().join("old");
    let current = directory.path().join("current");
    fs::create_dir(&old).unwrap();
    fs::create_dir(&current).unwrap();
    let old = old.join(if cfg!(windows) {
        "ez-assistant-runtime.exe"
    } else {
        "ez-assistant-runtime"
    });
    let current = current.join(if cfg!(windows) {
        "ez-assistant-runtime.exe"
    } else {
        "ez-assistant-runtime"
    });
    fs::copy(&binary, &old).unwrap();
    fs::copy(binary, &current).unwrap();
    let old = old.canonicalize().unwrap();
    let current = current.canonicalize().unwrap();
    let home = directory.path().join("home");
    fs::create_dir(&home).unwrap();
    #[cfg(unix)]
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let configuration = format!(
        "version = \"0.27.0\"\nmode = \"enterprise\"\n[enterprise]\ncenter_url = \"http://127.0.0.1:9\"\n[host_access]\nremote_enabled = false\nport = {port}\n"
    );
    fs::write(home.join("host.toml"), &configuration).unwrap();
    #[cfg(unix)]
    fs::set_permissions(home.join("host.toml"), fs::Permissions::from_mode(0o600)).unwrap();
    let mut child = tokio::process::Command::new(&old)
        .arg("serve")
        .arg("--runtime-home")
        .arg(&home)
        .arg("--password-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let coordinator = RuntimeBootstrapCoordinator::new(home.clone(), current.clone()).unwrap();
    let worker = coordinator.clone();
    let result = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now()+Duration::from_secs(10);
            while !home.join("run/runtime.lock").exists() || instance_lock_released(&home).unwrap_or(true) {
                assert!(tokio::time::Instant::now()<deadline,"early Host did not acquire lock");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(!home.join(super::super::DISCOVERY_RELATIVE_PATH).exists());
            let discovery = home.join(super::super::DISCOVERY_RELATIVE_PATH);
            let stale = |pid| serde_json::to_vec(&serde_json::json!({
                "address":"http://127.0.0.1:1", "instance_id":"early-old", "access_token":"a".repeat(43),
                "pid":pid,"executable_path":old,"executable_sha256":"obsolete-disk-hash",
            })).unwrap();
            fs::write(&discovery,stale(std::process::id())).unwrap();
            #[cfg(unix)]
            fs::set_permissions(&discovery,fs::Permissions::from_mode(0o600)).unwrap();
            assert!(worker.shutdown().await.is_err(),"stale PID must not authorize ending the lock owner");
            assert!(!instance_lock_released(&home).unwrap());
            fs::write(&discovery,stale(pid)).unwrap();
            // 同一 PID 如果晚于旧发现记录才创建，不得被当成旧实例结束。
            File::options().write(true).open(&discovery).unwrap()
                .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH)).unwrap();
            assert!(worker.shutdown().await.is_err());
            assert!(!instance_lock_released(&home).unwrap());
            fs::write(&discovery,stale(pid)).unwrap();
            // 模拟安装器已移动旧映像并将不同字节放回原路径，不能依赖旧路径／摘要不变。
            let moved = old.with_extension("moved");
            fs::rename(&old,&moved).unwrap();
            fs::copy(&current,&old).unwrap();
            {
                use std::io::Write as _;
                File::options().append(true).open(&old).unwrap().write_all(b"replacement-fixture").unwrap();
            }
            let unavailable = RuntimeBootstrapCoordinator::new(home.clone(),home.join("missing-bundle")).unwrap();
            assert!(matches!(unavailable.restart().await.unwrap_err().code,RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable));
            assert!(!instance_lock_released(&home).unwrap());
            let started = worker.restart().await.unwrap();
            let running = read_discovery(&home).unwrap();
            assert_eq!(running.executable_path,Some(current.clone()));
            assert_eq!(running.executable_sha256,Some(crate::runtime_source::digest(&current).unwrap()));
            assert_eq!(started.instance_id,running.instance_id);
            assert_ne!(running.pid,pid);
            assert_eq!(fs::read_to_string(home.join("host.toml")).unwrap(), configuration);
        }).await;
    let stopped = coordinator.shutdown().await;
    // 原始 serve 子进程一直由测试持有；即使断言失败也用 Child 的独占所有权回收。
    if child.try_wait().unwrap().is_none() {
        child.kill().await.unwrap();
    }
    child.wait().await.unwrap();
    if let Err(error) = stopped {
        let path = directory.keep();
        panic!("fixture retained at {}: {error}", path.display());
    }
    fn no_database(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                no_database(&path);
            } else {
                assert_ne!(path.file_name().unwrap(), "runtime.sqlite3");
            }
        }
    }
    no_database(&directory.path().join("home"));
    result.unwrap();
}
