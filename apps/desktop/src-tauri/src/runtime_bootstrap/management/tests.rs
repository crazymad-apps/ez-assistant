use super::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn status() -> HostAccessStatus {
    HostAccessStatus {
        mode: Some(HostMode::Enterprise),
        center_url: Some("https://saved.example/center".into()),
        center_id: None,
        revision: Some("saved-revision".into()),
        password_configured: false,
        configuration: Default::default(),
        listener_state: assistant_protocol::HostListenerState::Closed,
        restart_required: true,
        error: None,
    }
}

fn capabilities() -> RuntimeHostCapabilities {
    RuntimeHostCapabilities {
        mode: HostMode::Personal,
        platform: None,
        architecture: None,
        rg_on_path: None,
        runtime_version: assistant_protocol::SOFTWARE_VERSION.into(),
        min_compatible_version: assistant_protocol::MIN_COMPATIBLE_VERSION.into(),
        max_command_bytes: 1048576,
        max_attachment_bytes: None,
        sse: true,
        streaming_upload: true,
        features: REQUIRED_FEATURES.to_vec(),
    }
}

fn success() -> String {
    serde_json::json!({"result":{"scope":"host_access","payload":status()}}).to_string()
}

/// 有界脚本服务；每个测试 await 结束句柄，不遗留后台监听或访问真实 Host。
async fn server(replies: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (code, body) in replies {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0u8; 4096];
                let size = stream.read(&mut chunk).await.unwrap();
                assert!(size > 0);
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map(|value| value.parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            stream.write_all(format!(
                "HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
            ).as_bytes()).await.unwrap();
        }
        requests
    });
    (origin, task)
}

fn fixture(
    origin: &str,
) -> (
    tempfile::TempDir,
    RuntimeBootstrapCoordinator,
    RuntimeBootstrap,
) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("f02-management-")
        .tempdir_in(root)
        .unwrap();
    let run = directory.path().join("run");
    fs::create_dir(&run).unwrap();
    let path = run.join("runtime.json");
    fs::write(
        &path,
        serde_json::json!({
            "address": origin, "instance_id":"instance-a", "access_token":"a".repeat(43),
            "pid":std::process::id()
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let coordinator =
        RuntimeBootstrapCoordinator::new(directory.path().into(), PathBuf::new()).unwrap();
    let bootstrap = RuntimeBootstrap {
        base_url: origin.into(),
        instance_id: "instance-a".into(),
        access_token: "a".repeat(43),
        capabilities: capabilities(),
        started_runtime: false,
    };
    (directory, coordinator, bootstrap)
}

#[tokio::test]
async fn management_keeps_saved_enterprise_address_separate_from_running_personal_mode() {
    let (origin, server) = server(vec![
        (200, serde_json::to_string(&capabilities()).unwrap()),
        (
            200,
            serde_json::json!({"center_url":null,"enterprise_protocols":[]}).to_string(),
        ),
        (200, success()),
    ])
    .await;
    let (_home, coordinator, _) = fixture(&origin);
    let updater = crate::desktop_update::DesktopUpdater::default();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        coordinator.manage_host(&updater, HostAccessCommand::GetStatus, Some("instance-a")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.current_mode, HostMode::Personal);
    assert_eq!(result.current_center_url, None);
    assert_eq!(
        result.status.center_url.as_deref(),
        Some("https://saved.example/center")
    );
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["instance_id"], "instance-a");
    assert!(json.get("access_token").is_none());
    let requests = server.await.unwrap();
    assert!(requests[2].starts_with("POST /commands "));
    assert!(requests[2].contains("get_status"));
}

#[tokio::test]
async fn management_rejects_old_instance_before_configure_and_restart() {
    let (origin, server) = server(vec![
        (200, serde_json::to_string(&capabilities()).unwrap()),
        (200, serde_json::to_string(&capabilities()).unwrap()),
    ])
    .await;
    let (_home, coordinator, _) = fixture(&origin);
    let updater = crate::desktop_update::DesktopUpdater::default();
    let command = HostAccessCommand::Configure {
        expected_revision: None,
        configuration: Default::default(),
        mode: Some(HostMode::Personal),
        center_url: None,
        clear_center_binding: Some(true),
    };
    let error = coordinator
        .manage_host(&updater, command, Some("old-instance"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.code,
        RuntimeBootstrapErrorCode::ConfigurationConflict
    ));
    let error = coordinator
        .restart_checked(Some(&ExpectedHostConfiguration {
            instance_id: "old-instance".into(),
            revision: None,
        }))
        .await
        .unwrap_err();
    assert!(matches!(
        error.code,
        RuntimeBootstrapErrorCode::ConfigurationConflict
    ));
    assert!(
        server
            .await
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET /capabilities "))
    );
}

#[tokio::test]
async fn restart_guard_checks_revision_and_discovery_without_shutdown() {
    let (origin, server) = server(vec![(200, success()), (200, success())]).await;
    let (_home, coordinator, bootstrap) = fixture(&origin);
    let expected = ExpectedHostConfiguration {
        instance_id: "instance-a".into(),
        revision: Some("old".into()),
    };
    assert!(matches!(
        coordinator
            .verify_saved_configuration(&bootstrap, &expected)
            .await
            .unwrap_err()
            .code,
        RuntimeBootstrapErrorCode::ConfigurationConflict
    ));
    let expected = ExpectedHostConfiguration {
        revision: status().revision,
        ..expected
    };
    coordinator
        .verify_saved_configuration(&bootstrap, &expected)
        .await
        .unwrap();
    let mut replaced = bootstrap.clone();
    replaced.access_token = "other-token".into();
    assert!(coordinator.verify_management_target(&replaced).is_err());
    assert!(
        server
            .await
            .unwrap()
            .iter()
            .all(|request| request.contains("get_status") && !request.contains("shutdown"))
    );
}

#[tokio::test]
async fn management_preserves_known_rejections_and_redacts_unknown_responses() {
    for (body, expected) in [
        (
            r#"{"error":{"code":"configuration_conflict","message":"配置已变化。"}}"#,
            "configuration_conflict",
        ),
        (
            r#"{"error":{"code":"invalid_request","message":"地址无效。"}}"#,
            "invalid_request",
        ),
        (
            r#"{"error":{"code":"operation_not_allowed","message":"无管理权限。"}}"#,
            "operation_not_allowed",
        ),
        (
            r#"{"error":{"code":"configuration_unavailable","message":"secret-raw-response"}}"#,
            "configuration_unavailable",
        ),
        ("secret-raw-response", "configuration_unavailable"),
    ] {
        let (origin, task) = server(vec![(500, body.into())]).await;
        let response = reqwest::get(origin).await.unwrap();
        let error = decode_management_response(response).await.unwrap_err();
        let json = serde_json::to_value(error).unwrap();
        assert_eq!(json["code"], expected);
        assert!(!json.to_string().contains("secret-raw-response"));
        task.await.unwrap();
    }
    let (origin, task) = server(vec![(
        200,
        "{\"result\":{\"scope\":\"runtime\",\"payload\":{}}}".into(),
    )])
    .await;
    assert!(matches!(
        decode_management_response(reqwest::get(origin).await.unwrap())
            .await
            .unwrap_err()
            .code,
        RuntimeBootstrapErrorCode::ConfigurationUnavailable
    ));
    task.await.unwrap();
}

/// 显式提供已构建 Host，只创建本次独占 Home；企业模式未登录，不初始化用户库。
#[tokio::test]
#[ignore = "需要 EZ_ASSISTANT_TEST_HOST_BINARY；新建项目隔离 Home 并实际重启私有 Host 副本"]
async fn isolated_login_configuration_restarts_only_the_confirmed_host() {
    let binary = PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_HOST_BINARY").unwrap())
        .canonicalize()
        .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("f02-native-")
        .tempdir_in(root)
        .unwrap();
    let source = directory
        .path()
        .join(if cfg!(windows) { "host.exe" } else { "host" });
    fs::copy(binary, &source).unwrap();
    let home = directory.path().join("home");
    fs::create_dir(&home).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    fs::write(home.join("host.toml"), format!(
        "version = \"0.27.0\"\nmode = \"enterprise\"\n[enterprise]\ncenter_url = \"https://old.example/center\"\ncenter_id = \"01234567-89ab-4cde-8f01-23456789abcd\"\n[host_access]\nremote_enabled = false\nport = {port}\n"
    )).unwrap();
    drop(reservation);
    let coordinator = RuntimeBootstrapCoordinator::new(home.clone(), source).unwrap();
    let worker = coordinator.clone();
    // 观察任务 panic 后仍由测试 owner 关闭实际实例，不把私有 Host 遗留在后台。
    let result = tokio::spawn(async move {
        let updater = crate::desktop_update::DesktopUpdater::default();
        let before = worker
            .manage_host(&updater, HostAccessCommand::GetStatus, None)
            .await
            .unwrap();
        assert_eq!(before.current_mode, HostMode::Enterprise);
        assert!(before.status.center_id.is_some());
        let saved = worker
            .manage_host(
                &updater,
                HostAccessCommand::Configure {
                    expected_revision: before.status.revision,
                    configuration: before.status.configuration,
                    mode: Some(HostMode::Enterprise),
                    center_url: Some("https://new.example/center".into()),
                    clear_center_binding: Some(true),
                },
                Some(&before.instance_id),
            )
            .await
            .unwrap();
        assert!(saved.status.center_id.is_none());
        assert!(saved.status.restart_required);
        assert_eq!(
            saved.current_center_url.as_deref(),
            Some("https://old.example/center")
        );
        let expected = ExpectedHostConfiguration {
            instance_id: saved.instance_id.clone(),
            revision: saved.status.revision.clone(),
        };
        let stale_revision = ExpectedHostConfiguration {
            revision: Some("stale".into()),
            instance_id: saved.instance_id.clone(),
        };
        assert!(matches!(
            worker
                .restart_checked(Some(&stale_revision))
                .await
                .unwrap_err()
                .code,
            RuntimeBootstrapErrorCode::ConfigurationConflict
        ));
        assert_eq!(
            worker.discover(false).await.unwrap().instance_id,
            saved.instance_id
        );
        let restarted = worker.restart_checked(Some(&expected)).await.unwrap();
        assert_ne!(restarted.instance_id, saved.instance_id);
        let after = worker
            .manage_host(
                &updater,
                HostAccessCommand::GetStatus,
                Some(&restarted.instance_id),
            )
            .await
            .unwrap();
        assert!(!after.status.restart_required);
        assert_eq!(
            after.current_center_url.as_deref(),
            Some("https://new.example/center")
        );
        assert_eq!(after.status.center_url, after.current_center_url);
        // 原来源不可用时拒绝停止，不回退到 Desktop 随包来源。
        let discovery = read_discovery(&worker.runtime_home).unwrap();
        let source = discovery.executable_path.unwrap();
        let hidden = source.with_extension("unavailable");
        fs::rename(&source, &hidden).unwrap();
        let rejected = worker
            .restart_checked(Some(&ExpectedHostConfiguration {
                instance_id: restarted.instance_id.clone(),
                revision: after.status.revision,
            }))
            .await;
        fs::rename(hidden, source).unwrap();
        assert!(matches!(
            rejected.unwrap_err().code,
            RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable
        ));
        assert_eq!(
            worker.discover(false).await.unwrap().instance_id,
            restarted.instance_id
        );
    })
    .await;
    let stopped = coordinator.shutdown().await;
    if stopped.is_err() {
        let retained = directory.keep();
        panic!(
            "isolated Host shutdown not verified; fixture retained: {}",
            retained.display()
        );
    }
    assert!(instance_lock_released(&home).unwrap());
    fn assert_no_database(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                assert_no_database(&path);
            } else {
                assert_ne!(path.file_name().unwrap(), "runtime.sqlite3");
            }
        }
    }
    assert_no_database(&home);
    result.unwrap();
}
