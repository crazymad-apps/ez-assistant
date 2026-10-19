use super::{
    store::{Enrollment, Identity, Store, new_id, new_secret},
    *,
};
use crate::config_source::LocalConfigSource;
use std::{collections::BTreeMap, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("f01-identity-")
        .tempdir_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test"))
        .unwrap()
}
fn enrollment(intent: Intent) -> Enrollment {
    Enrollment {
        center_id: new_id().unwrap(),
        id: new_id().unwrap(),
        secret: new_secret().unwrap(),
        name: "本机".into(),
        platform: "test".into(),
        host_version: "0.32.0".into(),
        intent,
    }
}
async fn center(id: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = serde_json::json!({"center_id":id,"protocol_version":3,"min_protocol_version":3})
        .to_string();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /api/info "));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn switching_centers_preserves_each_registration_and_failed_save_keeps_configuration() {
    use assistant_protocol::{HostAccessCommand, HostAccessConfiguration};
    let home = home();
    let source = Arc::new(LocalConfigSource::new(home.path().join("host.toml")));
    let a = enrollment(Intent::Publish);
    let b = enrollment(Intent::Withdraw);
    let mut store = Store::open(&source).await.unwrap();
    store.identity = Some(Identity {
        installation_id: new_id().unwrap(),
        registrations: BTreeMap::from([
            (a.center_id.clone(), a.clone()),
            (b.center_id.clone(), b.clone()),
        ]),
    });
    store.save().await.unwrap();
    let identity_path = home.path().join("network/identity.json");
    let before = std::fs::read(&identity_path).unwrap();
    let mut document = crate::host_configuration::personal_document();
    crate::host_configuration::set_mode(
        &mut document,
        HostMode::Enterprise,
        Some("https://old.example"),
    )
    .unwrap();
    document["enterprise"]["center_id"] = toml_edit::value(&a.center_id);
    std::fs::write(source.path(), document.to_string()).unwrap();
    let permit = AccessPermit::new(true, None, CancellationToken::new());
    // 同中心换地址、切到另一中心、切回，以及同地址中心身份变化均由实际返回 ID 决定。
    for expected in [&a, &b, &a] {
        let (url, server) = center(&expected.center_id).await;
        if expected.center_id == b.center_id {
            let mut doc = std::fs::read_to_string(source.path())
                .unwrap()
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
            doc["enterprise"]["center_url"] = toml_edit::value(&url);
            std::fs::write(source.path(), doc.to_string()).unwrap();
        }
        let (mut service, _) = HostAccessService::new(source.clone());
        service.prepare().await.unwrap();
        let loaded = super::super::config::load(source.as_ref()).await.unwrap();
        let saved = service
            .execute(
                Some(HostAccessCommand::Configure {
                    expected_revision: loaded.revision,
                    configuration: HostAccessConfiguration::default(),
                    mode: Some(HostMode::Enterprise),
                    center_url: Some(url.clone()),
                    clear_center_binding: Some(true),
                }),
                &permit,
            )
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(saved.center_url.as_deref(), Some(url.as_str()));
        assert_eq!(
            saved.center_id.as_deref(),
            Some(expected.center_id.as_str())
        );
        assert!(saved.restart_required);
        let reloaded = Store::open(&source).await.unwrap();
        let status = status(&reloaded, saved.center_id);
        assert_eq!(
            status.registration_id.as_deref(),
            Some(expected.id.as_str())
        );
        assert_eq!(status.intent, Some(expected.intent));
        assert_eq!(std::fs::read(&identity_path).unwrap(), before);
    }
    let (mut service, _) = HostAccessService::new(source.clone());
    service.prepare().await.unwrap();
    let loaded = super::super::config::load(source.as_ref()).await.unwrap();
    let config_before = std::fs::read(source.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(
        service
            .execute(
                Some(HostAccessCommand::Configure {
                    expected_revision: loaded.revision.clone(),
                    configuration: HostAccessConfiguration::default(),
                    mode: Some(HostMode::Enterprise),
                    center_url: Some(unavailable),
                    clear_center_binding: Some(true),
                }),
                &permit
            )
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(source.path()).unwrap(), config_before);
    let saved = service
        .execute(
            Some(HostAccessCommand::Configure {
                expected_revision: loaded.revision,
                configuration: HostAccessConfiguration::default(),
                mode: Some(HostMode::Personal),
                center_url: None,
                clear_center_binding: Some(true),
            }),
            &permit,
        )
        .await
        .unwrap();
    assert_eq!(saved.mode, Some(HostMode::Personal));
    assert_eq!(std::fs::read(&identity_path).unwrap(), before);
}

#[tokio::test]
async fn identity_store_preserves_legacy_material_and_rejects_stale_or_damaged_writes() {
    let home = home();
    let source = LocalConfigSource::new(home.path().join("host.toml"));
    let enrollment = enrollment(Intent::Publish);
    let installation = new_id().unwrap();
    let dir = home.path().join("network");
    crate::config_source::prepare_private_directory(&dir).unwrap();
    let path = dir.join("identity.json");
    let legacy =
        serde_json::json!({"installation_id":installation,"current":enrollment}).to_string();
    use assistant_runtime::{ConfigSourceReplace, RuntimeConfigSource};
    assert!(matches!(
        LocalConfigSource::new(path.clone())
            .replace(None, legacy.clone())
            .await,
        ConfigSourceReplace::Applied(_)
    ));
    let mut first = Store::open(&source).await.unwrap();
    let mut stale = Store::open(&source).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy); // 只读不迁移
    let restored = first.identity.as_ref().unwrap();
    assert_eq!(restored.installation_id, installation);
    assert_eq!(
        restored.registrations[&enrollment.center_id]
            .secret
            .expose(),
        enrollment.secret.expose()
    );
    first.save().await.unwrap();
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("\"current\"")
    );
    assert!(matches!(stale.save().await, Err(AccessError::Conflict)));
    assert!(crate::platform::private_file_mode_is_secured(
        &std::fs::metadata(&path).unwrap()
    ));
    let mut next = Store::open(&source).await.unwrap();
    let other = self::enrollment(Intent::Closed);
    next.identity
        .as_mut()
        .unwrap()
        .registrations
        .insert(other.center_id.clone(), other);
    next.save().await.unwrap();
    assert_eq!(
        Store::open(&source)
            .await
            .unwrap()
            .identity
            .unwrap()
            .registrations
            .len(),
        2
    );
    std::fs::write(&path, "{broken").unwrap();
    assert!(Store::open(&source).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
}

#[tokio::test]
async fn changed_center_never_receives_previous_device_secret() {
    let enrollment = enrollment(Intent::Publish);
    let (url, server) = center(&new_id().unwrap()).await;
    let client = CenterClient::new(url).unwrap();
    let result = client
        .registration(
            &enrollment.center_id,
            &enrollment.id,
            &enrollment.secret,
            None,
        )
        .await;
    assert!(matches!(
        result,
        Err(super::super::enterprise::CenterError::IdentityMismatch)
    ));
    server.await.unwrap();
}
