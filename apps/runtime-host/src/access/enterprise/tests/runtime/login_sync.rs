use super::*;
use sha2::{Digest, Sha256};

// 真实插件解包共用进程级两个名额；这些用例验证登录语义，依次使用真实解包器。
static PACKAGES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn package(version: &str) -> PluginFixture {
    let manifest = json!({
        "format_version":1,"name":"warehouse-cad-guidance","display_name":"CAD",
        "description":"test","version":version,
        "platforms":[{"os":std::env::consts::OS,"arch":if cfg!(target_arch="aarch64") {"arm64"} else {"x64"}}],
        "min_host_version":"0.28.0","skills":["compact-shelving-cad"],"mcp":[],
        "configuration":[],"external_software":[]
    });
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for (name, bytes) in [
        ("plugin.json", manifest.to_string().into_bytes()),
        (
            "skills/compact-shelving-cad/SKILL.md",
            b"---\nname: compact-shelving-cad\ndescription: Test skill\n---\nTest.".to_vec(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    PluginFixture {
        version: version.into(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
        deprecated: false,
    }
}

async fn wait_plugins(
    runtime: &AssistantRuntime,
    predicate: impl Fn(&PluginListSnapshot) -> bool,
) -> PluginListSnapshot {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut events = runtime.subscribe_events();
        loop {
            let snapshot = runtime.list_plugins().await.unwrap();
            if predicate(&snapshot) {
                return snapshot;
            }
            events.recv().await.unwrap();
        }
    })
    .await;
    match result {
        Ok(snapshot) => snapshot,
        Err(_) => panic!("plugin wait timed out: {:?}", runtime.list_plugins().await),
    }
}

async fn install(
    f: &Fixture,
    token: &str,
    fixture: &PluginFixture,
) -> Arc<crate::http::ReadyServices> {
    let services = f.services(token).await;
    services
        .runtime
        .begin_plugin_install(BeginPluginInstallRequest {
            key: IdempotencyKey::new("initial-plugin").unwrap(),
            packages: vec![PluginPackageSelection {
                name: "warehouse-cad-guidance".into(),
                version: fixture.version.clone(),
                sha256: fixture.sha256.clone(),
                size_bytes: fixture.bytes.len() as u64,
                format: PluginArchiveFormat::TarGzip,
                origin: PluginPackageOrigin::Center {
                    center_id: CENTER_ID.into(),
                },
                expected_active_sha256: None,
            }],
        })
        .await
        .unwrap();
    let snapshot = wait_plugins(&services.runtime, |snapshot| {
        snapshot.operations.iter().any(|op| {
            matches!(
                op.phase,
                PluginOperationStatus::Succeeded
                    | PluginOperationStatus::Failed
                    | PluginOperationStatus::Interrupted
            )
        })
    })
    .await;
    assert_eq!(
        snapshot.operations[0].phase,
        PluginOperationStatus::Succeeded,
        "{snapshot:?}"
    );
    services
}

#[tokio::test]
async fn login_warms_domains_and_updates_installed_plugins_without_blocking_login() {
    let _packages = PACKAGES.lock().await;
    let mut f = Fixture::with_domains(true, None).await;
    let old = package("0.1.0");
    *f.mock.plugin.lock().unwrap() = Some(old.clone());
    let token = f.login("alice").await;
    // 没有调用 ensure-ready 或任何业务命令，登录自己触发目录请求。
    tokio::time::timeout(Duration::from_secs(10), f.mock.agent_requested.notified())
        .await
        .unwrap();
    assert_eq!(f.mock.agent_requests.load(Ordering::SeqCst), 1);
    let services = install(&f, &token, &old).await;
    let plugin = services
        .runtime
        .list_plugins()
        .await
        .unwrap()
        .plugins
        .remove(0);
    let request = MutatePluginSettingsRequest {
        name: plugin.name.clone(),
        expected_active_sha256: old.sha256.clone(),
        expected_revision: plugin.settings_revision,
        mutation: PluginSettingsMutation::SetEnabled { enabled: false },
    };
    loop {
        match services
            .runtime
            .mutate_plugin_settings(request.clone())
            .await
        {
            Ok(_) => break,
            Err(assistant_runtime::RuntimeError::PluginBusy) => tokio::task::yield_now().await,
            Err(error) => panic!("{error}"),
        }
    }
    let lease = services
        .runtime
        .plugin_config_lease(&plugin.name, &old.sha256)
        .await
        .unwrap();
    let next = package("0.2.0");
    *f.mock.plugin.lock().unwrap() = Some(next.clone());
    let second = f.login("alice").await;
    wait_plugins(&services.runtime, |snapshot| {
        snapshot.operations.iter().any(|op| {
            op.key.as_str().starts_with("login-plugin-")
                && op.phase == PluginOperationStatus::Preparing
        })
    })
    .await;
    assert_eq!(
        services.runtime.list_plugins().await.unwrap().plugins[0]
            .sha256
            .as_deref(),
        Some(old.sha256.as_str())
    );
    drop(lease);
    let updated = wait_plugins(&services.runtime, |snapshot| {
        snapshot.plugins[0].sha256.as_deref() == Some(next.sha256.as_str())
    })
    .await;
    assert!(!updated.plugins[0].enabled);
    assert_eq!(updated.plugins.len(), 1);
    assert_eq!(f.mock.agent_requests.load(Ordering::SeqCst), 2);
    assert_eq!(f.session(&second).await.status(), StatusCode::OK);
    assert_eq!(f.mock.agent_requests.load(Ordering::SeqCst), 2);
    assert!(f.mock.model_requests.load(Ordering::SeqCst) >= 2);
    f.stop().await;
}

#[tokio::test]
async fn late_plugin_resolution_from_previous_login_is_discarded() {
    let _packages = PACKAGES.lock().await;
    let mut f = Fixture::with_domains(true, None).await;
    let old = package("0.1.0");
    *f.mock.plugin.lock().unwrap() = Some(old.clone());
    let token = f.login("alice").await;
    f.mock.agent_requested.notified().await;
    let services = install(&f, &token, &old).await;
    let held = Arc::new(Held::default());
    *f.mock.plugin_resolve_hold.lock().unwrap() = Some(held.clone());
    *f.mock.plugin.lock().unwrap() = Some(package("0.2.0"));
    f.login("alice").await;
    tokio::time::timeout(Duration::from_secs(10), held.started.notified())
        .await
        .unwrap();
    let newest = package("0.3.0");
    *f.mock.plugin.lock().unwrap() = Some(newest.clone());
    f.login("alice").await;
    held.release.notify_one();
    let snapshot = wait_plugins(&services.runtime, |snapshot| {
        snapshot.plugins[0].sha256.as_deref() == Some(newest.sha256.as_str())
    })
    .await;
    assert_eq!(snapshot.operations.len(), 2, "旧登录不得接纳安装");
    f.stop().await;
}

#[tokio::test]
async fn failed_automatic_update_keeps_active_package_and_records_failure() {
    let _packages = PACKAGES.lock().await;
    let mut f = Fixture::with_domains(true, None).await;
    let old = package("0.1.0");
    *f.mock.plugin.lock().unwrap() = Some(old.clone());
    let token = f.login("alice").await;
    f.mock.agent_requested.notified().await;
    let services = install(&f, &token, &old).await;
    let mut invalid = package("0.2.0");
    invalid.bytes = b"invalid archive".to_vec();
    invalid.sha256 = format!("{:x}", Sha256::digest(&invalid.bytes));
    *f.mock.plugin.lock().unwrap() = Some(invalid);
    f.login("alice").await;
    let snapshot = wait_plugins(&services.runtime, |snapshot| {
        snapshot
            .operations
            .iter()
            .any(|op| op.phase == PluginOperationStatus::Failed)
    })
    .await;
    assert_eq!(
        snapshot.plugins[0].sha256.as_deref(),
        Some(old.sha256.as_str())
    );
    assert_eq!(
        snapshot.plugins[0].package_state,
        PluginPackageState::Available
    );
    assert!(snapshot.operations.iter().any(|op| op.error_code.is_some()));
    f.stop().await;
}
