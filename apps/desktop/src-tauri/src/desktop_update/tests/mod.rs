use super::*;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const MANIFEST: &str = include_str!("fixtures/release.json");
const SIGNATURE: &str = include_str!("fixtures/release.json.sig");
const KEY: &str = include_str!("fixtures/test-public-key.txt");
const ARTIFACT: &[u8] = include_bytes!("fixtures/artifact.txt");

#[tokio::test(start_paused = true)]
async fn checks_start_immediately_and_coalesce_sleep_without_overlapping() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let stop = CancellationToken::new();
    let (worker_count, worker_release, worker_stop) =
        (count.clone(), release.clone(), stop.clone());
    let task = tokio::spawn(async move {
        schedule::poll(&worker_stop, || async {
            if worker_count.fetch_add(1, Ordering::SeqCst) == 0 {
                worker_release.notified().await;
            }
        })
        .await;
    });
    tokio::task::yield_now().await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(30 * 60)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    release.notify_one();
    tokio::task::yield_now().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    tokio::time::advance(CHECK_INTERVAL - Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
    stop.cancel();
    task.await.unwrap();
}

fn remote_source(center: &str) -> UpdateSource {
    UpdateSource {
        binding: "selected".into(),
        center: Url::parse(center).unwrap(),
        host: ClientCompatibility::current(),
        protocols: vec![2],
        local: None,
        remote_origin: Some("http://127.0.0.1:7240".into()),
        cancellation: CancellationToken::new(),
    }
}

fn announcement(source: &UpdateSource) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
    serde_json::json!({
        "version": "0.29.1", "notes": "已修复问题", "pub_date": "2026-10-02T00:00:00Z",
        "url": source.endpoint("darwin-aarch64/releases/0.29.1/files/updater").unwrap(),
        "signature": value["files"][1]["signature"], "manifest": MANIFEST,
        "manifest_signature": SIGNATURE, "manifest_sha256": format!("{:x}", Sha256::digest(MANIFEST.as_bytes())),
    })
}

#[test]
fn signed_manifest_binds_version_target_url_and_artifact() {
    let source = remote_source("http://127.0.0.1:7320");
    let key = manifest::public_key(KEY).unwrap();
    let original = announcement(&source);
    let read = |json: &serde_json::Value, target: &str, current: &str| {
        manifest::VerifiedManifest::read(json, &key, &source, target, current, (2, 2))
    };
    assert!(read(&original, "darwin-aarch64", "0.29.0").is_ok());
    assert!(read(&original, "windows-x86_64", "0.29.0").is_err());
    assert!(read(&original, "darwin-aarch64", "0.29.1").is_err());
    assert!(read(&original, "darwin-aarch64", "0.29.10").is_err());
    for (field, replacement) in [
        ("version", serde_json::json!("99.0.0")),
        (
            "url",
            serde_json::json!("http://another-center.invalid/file"),
        ),
        (
            "manifest",
            serde_json::json!(MANIFEST.replace("0.29.1", "99.0.0")),
        ),
        ("manifest_sha256", serde_json::json!("0".repeat(64))),
        ("signature", serde_json::json!("modified")),
    ] {
        let mut changed = original.clone();
        changed[field] = replacement;
        assert!(
            read(&changed, "darwin-aarch64", "0.29.0").is_err(),
            "{field}"
        );
    }
}

#[test]
fn retained_remote_host_cannot_borrow_bundled_host_compatibility() {
    let mut source = remote_source("http://127.0.0.1:7320");
    source.host.version = "0.28.0".into();
    source.host.min_compatible_version = "0.28.0".into();
    assert!(
        manifest::VerifiedManifest::read(
            &announcement(&source),
            &manifest::public_key(KEY).unwrap(),
            &source,
            "darwin-aarch64",
            "0.29.0",
            (2, 2)
        )
        .is_err()
    );
    source.host = ClientCompatibility::current();
    source.protocols = vec![1];
    assert!(
        manifest::VerifiedManifest::read(
            &announcement(&source),
            &manifest::public_key(KEY).unwrap(),
            &source,
            "darwin-aarch64",
            "0.29.0",
            (2, 2)
        )
        .is_err()
    );
}

async fn server(body: Vec<u8>, declared: Option<usize>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]).to_lowercase();
        assert!(!request.contains("authorization:") && !request.contains("cookie:"));
        let length = declared
            .map(|n| format!("Content-Length: {n}\r\n"))
            .unwrap_or_default();
        stream
            .write_all(format!("HTTP/1.1 200 OK\r\n{length}Connection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let _ = stream.write_all(&body).await;
    });
    (origin, task)
}

#[tokio::test]
async fn download_verifies_stream_and_rejects_modified_truncated_or_oversized_bytes() {
    let key = manifest::public_key(KEY).unwrap();
    for (bytes, declared, valid) in [
        (ARTIFACT.to_vec(), Some(ARTIFACT.len()), true),
        (ARTIFACT.to_vec(), None, true),
        (vec![b'x'; ARTIFACT.len()], Some(ARTIFACT.len()), false),
        (ARTIFACT[..ARTIFACT.len() - 1].to_vec(), None, false),
        ([ARTIFACT, b"extra"].concat(), None, false),
        (ARTIFACT.to_vec(), Some(999999), false),
    ] {
        let (origin, server) = server(bytes, declared).await;
        let source = remote_source(&origin);
        let package = manifest::VerifiedManifest::read(
            &announcement(&source),
            &key,
            &source,
            "darwin-aarch64",
            "0.29.0",
            (2, 2),
        )
        .unwrap();
        let result = download::download(
            &source::http().unwrap(),
            &package,
            &key,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await;
        assert_eq!(result.is_ok(), valid);
        if let Ok(download) = result {
            assert_eq!(download.into_bytes().unwrap(), ARTIFACT);
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_download_makes_no_request() {
    let source = remote_source("http://127.0.0.1:1");
    let key = manifest::public_key(KEY).unwrap();
    let package = manifest::VerifiedManifest::read(
        &announcement(&source),
        &key,
        &source,
        "darwin-aarch64",
        "0.29.0",
        (2, 2),
    )
    .unwrap();
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(
        download::download(
            &source::http().unwrap(),
            &package,
            &key,
            &stop,
            |_, _| panic!("cancelled download progressed")
        )
        .await,
        Err(UpdateError::Cancelled)
    ));
}

#[tokio::test]
async fn cancelling_an_active_download_closes_the_stream() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = remote_source(&format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    ARTIFACT.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream
            .write_all(&ARTIFACT[..ARTIFACT.len() / 2])
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut request))
            .await
            .unwrap();
        assert!(
            matches!(result, Ok(0) | Err(_)),
            "download connection remained open"
        );
    });
    let key = manifest::public_key(KEY).unwrap();
    let package = manifest::VerifiedManifest::read(
        &announcement(&source),
        &key,
        &source,
        "darwin-aarch64",
        "0.29.0",
        (2, 2),
    )
    .unwrap();
    let stop = CancellationToken::new();
    let result = download::download(&source::http().unwrap(), &package, &key, &stop, |_, _| {
        stop.cancel()
    })
    .await;
    assert!(matches!(result, Err(UpdateError::Cancelled)));
    server.await.unwrap();
}

#[tokio::test]
async fn only_valid_protocol_ranges_can_report_incompatibility() {
    for (json, expected) in [
        (
            serde_json::json!({"min_protocol_version":2,"protocol_version":2}),
            Some(true),
        ),
        (
            serde_json::json!({"min_protocol_version":3,"protocol_version":4}),
            Some(false),
        ),
        (
            serde_json::json!({"min_protocol_version":0,"protocol_version":2}),
            None,
        ),
        (
            serde_json::json!({"min_protocol_version":4,"protocol_version":3}),
            None,
        ),
        (
            serde_json::json!({"min_protocol_version":2,"protocol_version":"3"}),
            None,
        ),
        (
            serde_json::json!({"min_protocol_version":65536,"protocol_version":65536}),
            None,
        ),
        (serde_json::json!({"protocol_version":3}), None),
        (
            serde_json::json!({"min_protocol_version":-1,"protocol_version":3}),
            None,
        ),
    ] {
        let bytes = serde_json::to_vec(&json).unwrap();
        let (origin, server) = server(bytes.clone(), Some(bytes.len())).await;
        let source = remote_source(&origin);
        assert_eq!(
            source
                .center_protocol()
                .await
                .ok()
                .map(|range| source.supports(range)),
            expected
        );
        server.await.unwrap();
    }
}

#[test]
fn update_admission_and_late_results_belong_to_the_selected_binding() {
    let old = remote_source("http://127.0.0.1:7320");
    let mut current = old.clone();
    current.binding = "reselected".into();
    current.cancellation = CancellationToken::new();
    let owner = DesktopUpdater::default();
    {
        let mut inner = owner.inner.lock().unwrap();
        inner.source = Some(current.clone());
        inner.snapshot.binding_id = Some(current.binding.clone());
        inner.snapshot.required = true;
        assert!(!active(&inner, &old));
        assert!(active(&inner, &current));
        current.cancellation.cancel();
        assert!(!active(&inner, &current));
    }
    assert!(owner.update_required("reselected"));
    assert!(!owner.update_required("selected"));
}
