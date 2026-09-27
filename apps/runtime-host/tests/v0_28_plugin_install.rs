//! 显式使用已保留的真实包，在新的产品 Host/SQLite 用户域验证上传安装与重启回读。

mod support;

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use reqwest::blocking::Client;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

#[test]
#[ignore = "set C03_SAMPLE_ARCHIVE to the preserved valid TAR.GZ fixture"]
fn upload_real_package_through_product_host_and_restore_after_restart() {
    let source = PathBuf::from(std::env::var_os("C03_SAMPLE_ARCHIVE").expect("sample archive"));
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let mut isolated = tempfile::Builder::new()
        .prefix("c03-m1-product-host-")
        .tempdir_in(base)
        .unwrap();
    isolated.disable_cleanup(true);
    let home = isolated.path();
    // 目标为新建的本机 SQLite 用户库；无源库和远端端口。
    // 应用实际读写 plugins、plugin_operations，预计各新增一行，既有行数为零。
    let host = support::HostProcess::start(home);
    let database = home.join("users/_personal/data/runtime.sqlite3");
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for table in ["plugins", "plugin_operations"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "fresh fixture table {table}");
    }
    drop(db);
    let mut commands = host.connect();
    let key = "c03-real-upload-install";
    let sha = "8e65947a8315c382db4f43ad36d6bb4423a292a8886ec7e5ba7b88c34bbacc52";
    let receipt = commands.runtime(
        "begin_plugin_install",
        json!({"key":key,"packages":[{
            "name":"warehouse-cad-guidance", "version":"0.1.0", "sha256":sha,
            "size_bytes":7_852_108, "format":"tar_gzip", "origin":{"type":"upload"},
            "expected_active_sha256":null
        }]}),
    );
    assert_eq!(receipt["phase"], "accepted");
    let http = Client::builder()
        .default_headers(support::compatibility_headers())
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap();
    let inspected = http
        .post(format!("{}/plugins/inspect", host.base_url()))
        .bearer_auth(host.access_token())
        .header("x-ez-plugin-format", "tar_gzip")
        .body(fs::read(&source).unwrap())
        .send()
        .unwrap();
    assert!(
        inspected.status().is_success(),
        "{}",
        inspected.text().unwrap()
    );
    let inspected: serde_json::Value = inspected.json().unwrap();
    assert_eq!(inspected["name"], "warehouse-cad-guidance");
    assert_eq!(inspected["version"], "0.1.0");
    assert_eq!(inspected["sha256"], sha);
    assert_eq!(inspected["size_bytes"], 7_852_108);
    assert_eq!(inspected["origin"]["type"], "upload");
    assert_eq!(
        fs::read_dir(home.join("users/_personal"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("plugin-inspect-"))
            .count(),
        0
    );
    let response = http
        .post(format!(
            "{}/plugins/operations/{key}/package",
            host.base_url()
        ))
        .bearer_auth(host.access_token())
        .body(fs::read(&source).unwrap())
        .send()
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let final_receipt = loop {
        let current = commands.runtime("get_plugin_operation", json!({"key":key}));
        if current["phase"] == "succeeded" {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "install did not settle: {current}"
        );
        assert_ne!(current["phase"], "failed", "install failed: {current}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        final_receipt["committed_names"],
        json!(["warehouse-cad-guidance"])
    );
    let list = commands.runtime("list_plugins", json!({}));
    assert_eq!(list["plugins"][0]["sha256"], sha);
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for table in ["plugins", "plugin_operations"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "committed fixture table {table}");
    }
    let active: String = db
        .query_row(
            "SELECT active_sha256 FROM plugins WHERE name='warehouse-cad-guidance'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(active, sha);
    drop(db);
    let user_plugins = home.join("users/_personal/plugins");
    assert_eq!(
        fs::read_dir(user_plugins.join(".staging")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(user_plugins.join(".trash")).unwrap().count(),
        0
    );
    assert!(
        user_plugins
            .join(format!("warehouse-cad-guidance/packages/{sha}/plugin.json"))
            .is_file()
    );
    let _ = host.kill();
    let restarted = support::HostProcess::start(home);
    let mut commands = restarted.connect();
    let restored = commands.runtime("list_plugins", json!({}));
    assert_eq!(restored["plugins"][0]["sha256"], sha);
    let manifest = user_plugins.join(format!("warehouse-cad-guidance/packages/{sha}/plugin.json"));
    let held_manifest = manifest.with_extension("json.audit-backup");
    let _ = restarted.kill();
    fs::rename(&manifest, &held_manifest).unwrap();
    let degraded = support::HostProcess::start(home);
    let mut commands = degraded.connect();
    let degraded_list = commands.runtime("list_plugins", json!({}));
    assert_eq!(degraded_list["plugins"][0]["package_state"], "corrupt");
    assert_eq!(degraded_list["cleanup_blocked"], false);
    let _ = degraded.kill();
    fs::rename(held_manifest, manifest).unwrap();
    let restarted = support::HostProcess::start(home);
    let mut commands = restarted.connect();
    assert_eq!(
        commands.runtime("list_plugins", json!({}))["plugins"][0]["package_state"],
        "available"
    );
    let native_config = b"[native]\nvalue=\xff\0\n".to_vec();
    let uploaded = http
        .put(format!(
            "{}/plugins/warehouse-cad-guidance/config-files/settings.ini",
            restarted.base_url()
        ))
        .bearer_auth(restarted.access_token())
        .header("x-ez-plugin-sha256", sha)
        .header("x-ez-expected-revision", "absent")
        .body(native_config.clone())
        .send()
        .unwrap();
    assert!(
        uploaded.status().is_success(),
        "{}",
        uploaded.text().unwrap()
    );
    let listed = http
        .get(format!(
            "{}/plugins/warehouse-cad-guidance/config-files?sha256={sha}",
            restarted.base_url()
        ))
        .bearer_auth(restarted.access_token())
        .send()
        .unwrap();
    assert!(listed.status().is_success());
    let files: serde_json::Value = listed.json().unwrap();
    assert_eq!(files["files"][0]["path"], "settings.ini");
    assert_eq!(files["files"][0]["size_bytes"], native_config.len() as u64);
    let stale = http
        .put(format!(
            "{}/plugins/warehouse-cad-guidance/config-files/settings.ini",
            restarted.base_url()
        ))
        .bearer_auth(restarted.access_token())
        .header("x-ez-plugin-sha256", sha)
        .header("x-ez-expected-revision", "absent")
        .body(b"stale".to_vec())
        .send()
        .unwrap();
    assert!(!stale.status().is_success());
    assert_eq!(
        fs::read(user_plugins.join("warehouse-cad-guidance/config/settings.ini")).unwrap(),
        native_config
    );
    // 删除生效引用和包文件前，在另一个隔离目录创建并核验独立在线备份。
    let mut backup_dir = tempfile::Builder::new()
        .prefix("c03-m1-backup-")
        .tempdir_in(home.parent().unwrap())
        .unwrap();
    backup_dir.disable_cleanup(true);
    let backup = backup_dir.path().join("before-uninstall.sqlite3");
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.backup(rusqlite::MAIN_DB, &backup, None).unwrap();
    assert!(backup.is_file());
    let saved = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for (table, expected) in [("plugins", 1), ("plugin_operations", 1)] {
        let count: i64 = saved
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, expected, "verified backup table {table}");
    }
    let saved_sha: String = saved
        .query_row(
            "SELECT active_sha256 FROM plugins WHERE name='warehouse-cad-guidance'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved_sha, sha);
    drop(saved);
    drop(db);
    let uninstall_key = "c03-real-default-uninstall";
    commands.runtime("uninstall_plugin", json!({"key":uninstall_key,"name":"warehouse-cad-guidance","expected_active_sha256":sha,"clear_data":false}));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let current = commands.runtime("get_plugin_operation", json!({"key":uninstall_key}));
        if current["phase"] == "succeeded" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "uninstall did not settle: {current}"
        );
        assert_ne!(current["phase"], "failed", "uninstall failed: {current}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let plugins = commands.runtime("list_plugins", json!({}));
    assert!(plugins["plugins"][0]["sha256"].is_null());
    assert!(
        !user_plugins
            .join(format!("warehouse-cad-guidance/packages/{sha}"))
            .exists()
    );
    assert!(user_plugins.join("warehouse-cad-guidance/config").is_dir());
    assert!(user_plugins.join("warehouse-cad-guidance/data").is_dir());
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let plugin_count: i64 = db
        .query_row("SELECT COUNT(*) FROM plugins", [], |row| row.get(0))
        .unwrap();
    let operation_count: i64 = db
        .query_row("SELECT COUNT(*) FROM plugin_operations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((plugin_count, operation_count), (1, 2));
    drop(db);
    assert_eq!(
        fs::read_dir(user_plugins.join(".staging")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(user_plugins.join(".trash")).unwrap().count(),
        0
    );
    let config_file = user_plugins.join("warehouse-cad-guidance/config/settings.ini");
    let data_file = user_plugins.join("warehouse-cad-guidance/data/result.txt");
    assert_eq!(fs::read(&config_file).unwrap(), native_config);
    fs::write(&data_file, "retained data").unwrap();
    let reinstall_key = "c03-real-reinstall";
    commands.runtime(
        "begin_plugin_install",
        json!({"key":reinstall_key,"packages":[{
            "name":"warehouse-cad-guidance", "version":"0.1.0", "sha256":sha,
            "size_bytes":7_852_108, "format":"tar_gzip", "origin":{"type":"upload"},
            "expected_active_sha256":null
        }]}),
    );
    let response = http
        .post(format!(
            "{}/plugins/operations/{reinstall_key}/package",
            restarted.base_url()
        ))
        .bearer_auth(restarted.access_token())
        .body(fs::read(&source).unwrap())
        .send()
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let current = commands.runtime("get_plugin_operation", json!({"key":reinstall_key}));
        if current["phase"] == "succeeded" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reinstall did not settle: {current}"
        );
        assert_ne!(current["phase"], "failed", "reinstall failed: {current}");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fs::read(&config_file).unwrap(), native_config);
    assert_eq!(fs::read_to_string(&data_file).unwrap(), "retained data");
    let clear_backup = backup_dir.path().join("before-clear.sqlite3");
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.backup(rusqlite::MAIN_DB, &clear_backup, None).unwrap();
    assert!(clear_backup.is_file());
    let saved =
        Connection::open_with_flags(&clear_backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for (table, expected) in [("plugins", 1), ("plugin_operations", 3)] {
        let count: i64 = saved
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, expected, "verified clear backup table {table}");
    }
    let saved_sha: String = saved
        .query_row(
            "SELECT active_sha256 FROM plugins WHERE name='warehouse-cad-guidance'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved_sha, sha);
    drop(saved);
    drop(db);
    let clear_key = "c03-real-clear-uninstall";
    commands.runtime("uninstall_plugin", json!({"key":clear_key,"name":"warehouse-cad-guidance","expected_active_sha256":sha,"clear_data":true}));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let current = commands.runtime("get_plugin_operation", json!({"key":clear_key}));
        if current["phase"] == "succeeded" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "clear uninstall did not settle: {current}"
        );
        assert_ne!(
            current["phase"], "failed",
            "clear uninstall failed: {current}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let db = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    for (table, expected) in [("plugins", 0), ("plugin_operations", 4)] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, expected, "after clear table {table}");
    }
    drop(db);
    assert!(!user_plugins.join("warehouse-cad-guidance").exists());
    assert_eq!(
        fs::read_dir(user_plugins.join(".staging")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(user_plugins.join(".trash")).unwrap().count(),
        0
    );
    let _ = restarted.kill();
    eprintln!("retained C03 isolated Host fixture: {}", home.display());
    eprintln!("verified independent backup: {}", backup.display());
    eprintln!("verified clear backup: {}", clear_backup.display());
}
