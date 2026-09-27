//! Manual acceptance of a built CAD plugin archive through the product Host.

mod support;

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
#[ignore = "set C03_CAD_ARCHIVE to a built macOS arm64 CAD plugin TAR.GZ"]
fn bundled_cad_plugin_installs_in_a_fresh_user_domain() {
    let archive = PathBuf::from(std::env::var_os("C03_CAD_ARCHIVE").expect("C03_CAD_ARCHIVE"));
    let bytes = fs::read(&archive).expect("built CAD archive");
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let isolated = tempfile::Builder::new()
        .prefix("c03-m5-cad-plugin-")
        .tempdir_in(base)
        .unwrap();
    let home = isolated.path();

    // This new local SQLite database has no source database and no existing plugin rows.
    let host = support::HostProcess::start(home);
    let database = home.join("users/_personal/data/runtime.sqlite3");
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for table in ["plugins", "plugin_operations"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "fresh {table}");
    }
    drop(db);

    let client = Client::builder()
        .default_headers(support::compatibility_headers())
        .timeout(Duration::from_secs(180))
        .build()
        .unwrap();
    let inspected = client
        .post(format!("{}/plugins/inspect", host.base_url()))
        .bearer_auth(host.access_token())
        .header("x-ez-plugin-format", "tar_gzip")
        .body(bytes.clone())
        .send()
        .unwrap();
    assert!(
        inspected.status().is_success(),
        "{}",
        inspected.text().unwrap()
    );
    let inspected: serde_json::Value = inspected.json().unwrap();
    assert_eq!(inspected["name"], "warehouse-cad-guidance");
    assert_eq!(inspected["version"], "0.3.2");
    assert_eq!(inspected["sha256"], sha);

    let mut commands = host.connect();
    let key = "c03-m5-bundled-cad-install";
    commands.runtime(
        "begin_plugin_install",
        json!({"key":key,"packages":[{
            "name":"warehouse-cad-guidance", "version":"0.3.2", "sha256":sha,
            "size_bytes":bytes.len(), "format":"tar_gzip", "origin":{"type":"upload"},
            "expected_active_sha256":null
        }]}),
    );
    let uploaded = client
        .post(format!(
            "{}/plugins/operations/{key}/package",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .body(bytes)
        .send()
        .unwrap();
    assert!(
        uploaded.status().is_success(),
        "{}",
        uploaded.text().unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let receipt = commands.runtime("get_plugin_operation", json!({"key":key}));
        if receipt["phase"] == "succeeded" {
            break;
        }
        assert_ne!(receipt["phase"], "failed", "{receipt}");
        assert!(Instant::now() < deadline, "install timed out: {receipt}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let installed = commands.runtime("list_plugins", json!({}));
    assert_eq!(installed["plugins"][0]["sha256"], sha);
    assert_eq!(installed["plugins"][0]["mcp"], json!(["autocad-ai"]));
    assert_eq!(installed["plugins"][0]["external_software"], json!([]));
    let package = home
        .join("users/_personal/plugins/warehouse-cad-guidance/packages")
        .join(&sha);
    assert!(
        home.join("users/_personal/plugins/warehouse-cad-guidance/config")
            .is_dir()
    );
    assert!(
        home.join("users/_personal/plugins/warehouse-cad-guidance/data")
            .is_dir()
    );
    assert!(package.join("mcp/autocad-ai/server.json").is_file());
    assert!(
        package
            .join("runtimes/macos-arm64/python/bin/python3.12")
            .is_file()
    );
    assert!(
        package
            .join("skills/compact-shelving-cad/SKILL.md")
            .is_file()
    );
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for table in ["plugins", "plugin_operations"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "installed {table}");
    }
    drop(db);
    let _ = host.kill();
}
