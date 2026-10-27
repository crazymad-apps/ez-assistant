use super::*;

#[cfg(unix)]
fn capabilities(version: &str) -> RuntimeHostCapabilities {
    serde_json::from_value(serde_json::json!({
        "runtime_version": version, "min_compatible_version": version,
        "max_command_bytes": 1024, "max_attachment_bytes": null,
        "sse": true, "streaming_upload": true, "features": REQUIRED_FEATURES,
    }))
    .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn upgrade_requires_replaced_same_installation_and_current_candidate() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().unwrap();
    let source = home.path().canonicalize().unwrap().join("host");
    fs::write(
        &source,
        format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            serde_json::to_string(&ClientCompatibility::current()).unwrap()
        ),
    )
    .unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let coordinator =
        RuntimeBootstrapCoordinator::new(home.path().to_owned(), source.clone()).unwrap();
    let original = RuntimeDiscovery {
        address: "http://127.0.0.1:7240".into(),
        instance_id: "old".into(),
        access_token: "a".repeat(43),
        pid: std::process::id(),
        executable_path: Some(source.clone()),
        executable_sha256: Some("a".repeat(64)),
    };
    let old = capabilities("0.26.0");
    assert!(coordinator.upgrade_source(&original, &old).await.is_ok());
    for version in ["99.0.0", "invalid"] {
        assert!(
            coordinator
                .upgrade_source(&original, &capabilities(version))
                .await
                .is_err()
        );
    }
    for version in [
        "0.26.0",
        "0.27.0",
        "0.32.0",
        assistant_protocol::SOFTWARE_VERSION,
    ] {
        let mut previous = capabilities(version);
        previous.mode = assistant_protocol::HostMode::Enterprise;
        assert!(
            coordinator
                .upgrade_source(&original, &previous)
                .await
                .is_ok()
        );
    }
    let mut identical = original.clone();
    identical.executable_sha256 = Some(crate::runtime_source::digest(&source).unwrap());
    assert!(
        coordinator
            .upgrade_source(
                &identical,
                &capabilities(assistant_protocol::SOFTWARE_VERSION)
            )
            .await
            .is_err()
    );
    let mut unrelated = original.clone();
    unrelated.executable_path = Some(home.path().join("other-installation"));
    assert!(coordinator.upgrade_source(&unrelated, &old).await.is_err());
    unrelated = original.clone();
    unrelated.executable_sha256 = None;
    assert!(coordinator.upgrade_source(&unrelated, &old).await.is_err());
    fs::write(
        &source,
        "#!/bin/sh\nprintf '%s' '{\"version\":\"0.26.0\",\"min_compatible_version\":\"0.26.0\"}'\n",
    )
    .unwrap();
    assert!(coordinator.upgrade_source(&original, &old).await.is_err());
}

/// 手动准备真实 0.26.0 Host 后替换安装文件；测试只访问带标记的项目隔离目录。
#[tokio::test]
#[ignore = "需要显式准备同来源旧版运行进程与新版安装文件"]
async fn isolated_installed_host_upgrade_and_duplicate_request() {
    let root = PathBuf::from(std::env::var_os("EZ_ASSISTANT_UPGRADE_FIXTURE").expect("fixture"))
        .canonicalize()
        .unwrap();
    let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.runtime-test")
        .canonicalize()
        .unwrap();
    assert!(root.starts_with(allowed));
    assert_eq!(
        fs::read_to_string(root.join("upgrade-fixture")).unwrap(),
        "0.26.0-to-0.27.0"
    );
    let home = root.join("home");
    let source = root.join(format!(
        "ez-assistant-runtime{}",
        std::env::consts::EXE_SUFFIX
    ));
    let coordinator = RuntimeBootstrapCoordinator::new(home.clone(), source).unwrap();
    let before = read_discovery(&home).unwrap();
    assert!(matches!(
        coordinator.bootstrap().await.unwrap_err().code,
        RuntimeBootstrapErrorCode::RuntimeUpgradeRequired
    ));
    assert_eq!(
        read_discovery(&home).unwrap().instance_id,
        before.instance_id
    );
    let upgraded = coordinator.upgrade().await.unwrap();
    assert_ne!(upgraded.instance_id, before.instance_id);
    assert_eq!(
        upgraded.capabilities.runtime_version,
        assistant_protocol::SOFTWARE_VERSION
    );
    assert_eq!(
        coordinator.upgrade().await.unwrap().instance_id,
        upgraded.instance_id
    );
    coordinator.shutdown().await.unwrap();
}
