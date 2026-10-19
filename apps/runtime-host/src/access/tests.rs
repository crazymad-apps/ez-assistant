//! 地址准入与本机权限分离；不依赖数据库或真实监听。

use super::*;

#[tokio::test]
async fn local_status_is_available_while_center_request_is_pending() {
    let home = tempfile::Builder::new()
        .prefix("login-status-")
        .tempdir_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test"))
        .unwrap();
    let path = home.path().join("host.toml");
    std::fs::write(
        &path,
        crate::host_configuration::personal_document().to_string(),
    )
    .unwrap();
    let (mut service, handle) = HostAccessService::new(Arc::new(LocalConfigSource::new(path)));
    service.prepare().await.unwrap();
    let mut queries = service.queries.take().unwrap();
    let finished = tokio::sync::Notify::new();
    let (status, ()) = tokio::join!(
        async {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                handle.command(
                    Some(HostAccessCommand::GetStatus),
                    AccessPermit::new(true, None, CancellationToken::new()),
                ),
            )
            .await;
            finished.notify_one();
            result
                .expect("local configuration must not wait for the center")
                .unwrap()
        },
        service.with_status_queries(finished.notified(), &mut queries),
    );
    assert_eq!(status.mode, Some(assistant_protocol::HostMode::Personal));
    assert!(!status.restart_required);
}

#[test]
fn remote_ip_access_includes_forwarded_loopback_but_requires_enabled_policy() {
    let (commands, _receive) = mpsc::channel(1);
    let credentials = Arc::new(Credentials::new());
    let handle = HostAccessHandle {
        network: registration::network::Network::new(credentials.clone()),
        protected_files: Arc::new(RwLock::new(Vec::new())),
        center: Arc::new(RwLock::new(None)),
        binding: mpsc::channel(1).0,
        registration: mpsc::channel(1).0,
        credentials,
        commands,
        queries: mpsc::channel(1).0,
        remote: Arc::new(RwLock::new(RemoteAccess::default())),
    };
    let addresses = [
        "127.0.0.1",
        "127.0.0.2",
        "[::1]",
        "192.0.2.10",
        "[2001:db8::1]",
        "0.0.0.0",
        "[::]",
    ];
    for address in addresses {
        assert!(matches!(
            handle.remote_connection(address),
            Err(AccessError::Unauthorized)
        ));
    }
    {
        let mut remote = handle.remote.write().unwrap();
        remote.enabled = true;
        remote
            .server_names
            .extend(["RUNTIME.TEST".into(), "例子.测试".into()]);
    }
    for address in addresses {
        assert!(handle.remote_connection(address).is_ok(), "{address}");
    }
    assert!(handle.remote_connection("runtime.test").is_ok());
    assert!(handle.remote_connection("xn--fsqu00a.xn--0zwm56d").is_ok());
    for address in ["unknown.test", "localhost"] {
        assert!(matches!(
            handle.remote_connection(address),
            Err(AccessError::Unauthorized)
        ));
    }
    let granted = handle.remote_connection("127.0.0.1").unwrap();
    handle.remote.write().unwrap().disable();
    assert!(granted.is_cancelled());
    assert!(handle.remote_connection("127.0.0.1").is_err());
}
