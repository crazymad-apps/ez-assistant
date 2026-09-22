use super::*;
use std::io::{Read as _, Write as _};

fn capabilities(version: &str) -> RuntimeHostCapabilities {
    serde_json::from_value(serde_json::json!({
        "runtime_version": version, "min_compatible_version": version,
        "max_command_bytes": 1024, "max_attachment_bytes": null,
        "sse": true, "streaming_upload": true, "features": REQUIRED_FEATURES,
    }))
    .unwrap()
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .map(|n| n.parse::<usize>().unwrap())
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn upgrade_shutdown_keeps_real_client_version_and_only_accepts_shutdown_ack() {
    for (status, body, accepted) in [
        (
            "200 OK",
            r#"{"result":{"scope":"runtime","payload":{"type":"shutdown_runtime","payload":{"lifecycle":"shutting_down"}}}}"#,
            true,
        ),
        (
            "200 OK",
            r#"{"result":{"scope":"runtime","payload":{"type":"shutdown_runtime","payload":{"lifecycle":"running"}}}}"#,
            false,
        ),
        ("409 Conflict", "{}", false),
        ("200 OK", "{}", false),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let (headers, request_body) = request.split_once("\r\n\r\n").unwrap();
            let headers = headers.to_ascii_lowercase();
            assert!(headers.starts_with("post /commands "));
            assert!(headers.contains(&format!(
                "x-ez-client-version: {}\r\n",
                assistant_protocol::SOFTWARE_VERSION
            )));
            assert!(headers.contains("x-ez-min-compatible-version: 0.26.0"));
            assert!(headers.contains("authorization: bearer fixed-instance-token"));
            let json: serde_json::Value = serde_json::from_str(request_body).unwrap();
            assert_eq!(
                json["command"],
                serde_json::json!({"scope":"runtime","payload":{"type":"shutdown_runtime","payload":{}}})
            );
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let coordinator =
            RuntimeBootstrapCoordinator::new(home.path().to_owned(), home.path().join("absent"))
                .unwrap();
        let previous = RuntimeBootstrap {
            base_url: address,
            instance_id: "fixed".into(),
            access_token: "fixed-instance-token".into(),
            capabilities: capabilities("0.26.0"),
            started_runtime: false,
        };
        assert_eq!(
            coordinator.stop_for_upgrade(&previous).await.is_ok(),
            accepted
        );
        server.join().unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn upgrade_requires_same_installed_source_known_predecessor_and_current_candidate() {
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
    for version in ["0.25.3", "0.27.0", "0.28.0", "invalid"] {
        assert!(
            coordinator
                .upgrade_source(&original, &capabilities(version))
                .await
                .is_err()
        );
    }
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
