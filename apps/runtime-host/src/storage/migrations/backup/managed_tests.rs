//! 受管文件配套备份与隔离恢复验证。

use super::*;

fn home() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f02-backup-")
        .tempdir_in(fs::canonicalize(base).unwrap())
        .unwrap()
}

fn fixture(home: &Path) -> (PathBuf, Connection) {
    let data = home.join("data");
    fs::create_dir_all(data.join("sessions/session-one")).unwrap();
    let path = data.join("runtime.sqlite3");
    let source = Connection::open(&path).unwrap();
    source.execute_batch("CREATE TABLE fixture(id INTEGER PRIMARY KEY, value TEXT NOT NULL); INSERT INTO fixture VALUES(1,'保留'),(2,'原始');").unwrap();
    fs::write(
        data.join("sessions/session-one/conversation.1.jsonl"),
        b"original history\n",
    )
    .unwrap();
    fs::write(home.join("config.toml"), b"default_model = 'before'\n").unwrap();
    (path, source)
}

#[test]
fn history_backup_restores_matching_database_files_and_config_independently() {
    let home = home();
    let (path, mut source) = fixture(home.path());
    let snapshot = source.transaction().unwrap();
    let backup = create(&snapshot, home.path(), &path, "0.32.1").unwrap();
    snapshot.commit().unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["external_files_included"], true);
    assert_eq!(manifest["file_scope"], "session_history_only");
    assert_eq!(manifest["database"]["tables"]["fixture"]["rows"], 2);
    assert!(
        manifest["managed_files"]
            .get("data/runtime.sqlite3")
            .is_none()
    );
    let relative = Path::new("data/sessions/session-one/conversation.1.jsonl");
    assert_eq!(
        fs::read(backup.join(relative)).unwrap(),
        b"original history\n"
    );
    // 故意修改源，证明正文和数据库均非硬链接或继续读取源文件的伪备份。
    source
        .execute("UPDATE fixture SET value='after' WHERE id=1", [])
        .unwrap();
    fs::write(home.path().join(relative), b"new history\n").unwrap();
    fs::write(
        home.path().join("config.toml"),
        b"default_model = 'after'\n",
    )
    .unwrap();
    assert_eq!(
        fs::read(backup.join(relative)).unwrap(),
        b"original history\n"
    );

    // 恢复到另一个新建且无用户数据的隔离 Home，原源库继续保持 after。
    let restore = self::home();
    fs::create_dir_all(restore.path().join("data")).unwrap();
    fs::copy(
        backup.join("runtime.sqlite3"),
        restore.path().join("data/runtime.sqlite3"),
    )
    .unwrap();
    let entries = managed_inventory(&backup).unwrap();
    for (relative, entry) in &entries {
        if relative == Path::new("data") {
            continue;
        }
        files::copy_entry(
            &backup.join(relative),
            &restore.path().join(relative),
            entry,
        )
        .unwrap();
    }
    fs::copy(
        backup.join("config.toml"),
        restore.path().join("config.toml"),
    )
    .unwrap();
    let restored = Connection::open_with_flags(
        restore.path().join("data/runtime.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    check_integrity(&restored).unwrap();
    assert_eq!(
        restored
            .query_row("SELECT COUNT(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        restored
            .query_row("SELECT value FROM fixture WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "保留"
    );
    assert_eq!(
        fs::read(restore.path().join(relative)).unwrap(),
        b"original history\n"
    );
    assert_eq!(
        fs::read(restore.path().join("config.toml")).unwrap(),
        b"default_model = 'before'\n"
    );
    assert_eq!(managed_inventory(restore.path()).unwrap(), entries);
    assert_eq!(
        source
            .query_row("SELECT value FROM fixture WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "after"
    );
}

#[test]
fn damaged_history_is_backed_up_as_bytes_without_pretending_it_is_healthy() {
    let home = home();
    let (path, source) = fixture(home.path());
    let relative = Path::new("data/sessions/session-one/conversation.1.jsonl");
    let damaged = b"{\"unterminated history";
    fs::write(home.path().join(relative), damaged).unwrap();
    let saved = create(&source, home.path(), &path, "0.32.1").unwrap();
    assert_eq!(fs::read(saved.join(relative)).unwrap(), damaged);
    assert_eq!(
        managed_inventory(&saved).unwrap(),
        managed_inventory(home.path()).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn file_backup_preserves_links_without_copying_external_targets() {
    use std::os::unix::fs::symlink;
    let home = home();
    let external = self::home();
    let (path, source) = fixture(home.path());
    fs::write(
        external.path().join("private.txt"),
        b"not managed by this home",
    )
    .unwrap();
    let relative = Path::new("data/sessions/session-one/conversation.2.jsonl");
    symlink(external.path(), home.path().join(relative)).unwrap();
    let saved = create(&source, home.path(), &path, "0.32.1").unwrap();
    assert!(
        fs::symlink_metadata(saved.join(relative))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(saved.join(relative)).unwrap(),
        external.path()
    );
    let entries = managed_inventory(&saved).unwrap();
    assert!(!entries.keys().any(|path| path.ends_with("private.txt")));
}

#[cfg(unix)]
#[test]
fn unsupported_managed_entry_never_publishes_a_success_manifest_or_changes_source() {
    let home = home();
    let (path, source) = fixture(home.path());
    // FIFO 是不支持的历史文件类型；lstat 后应拒绝，不能阻塞读取，也不受 Unix socket 路径长度限制。
    assert!(
        std::process::Command::new("mkfifo")
            .arg(
                home.path()
                    .join("data/sessions/session-one/conversation.2.jsonl")
            )
            .status()
            .unwrap()
            .success()
    );
    assert!(create(&source, home.path(), &path, "0.32.1").is_err());
    for entry in fs::read_dir(home.path().join("backups/database")).unwrap() {
        assert!(!entry.unwrap().path().join("manifest.json").exists());
    }
    assert_eq!(
        source
            .query_row("SELECT COUNT(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        source
            .query_row("SELECT value FROM fixture WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "保留"
    );
    assert_eq!(
        fs::read(
            home.path()
                .join("data/sessions/session-one/conversation.1.jsonl")
        )
        .unwrap(),
        b"original history\n"
    );
}

#[test]
fn history_allowlist_excludes_projects_dependencies_resources_and_reports_real_progress() {
    use assistant_protocol::RuntimeHostStartupStage;
    let home = home();
    let (path, source) = fixture(home.path());
    let session = home.path().join("data/sessions/session-one");
    for name in [
        "private/.venv/lib",
        "private/node_modules/package",
        "attachments",
        "tool-images",
    ] {
        fs::create_dir_all(session.join(name)).unwrap();
        fs::write(
            session.join(name).join("conversation.1.jsonl"),
            b"excluded project/resource",
        )
        .unwrap();
    }
    let child = session.join("child-tasks/child-one");
    fs::create_dir_all(child.join("private/.venv")).unwrap();
    fs::write(child.join("body-1.jsonl"), b"child history").unwrap();
    fs::write(
        child.join("private/.venv/body-1.jsonl"),
        b"excluded dependency",
    )
    .unwrap();
    let mut updates = Vec::new();
    let saved = create_with_progress(&source, home.path(), &path, "0.32.1", |stage, progress| {
        updates.push((stage, progress))
    })
    .unwrap();
    let inventory = managed_inventory(&saved).unwrap();
    assert!(inventory.contains_key(Path::new(
        "data/sessions/session-one/child-tasks/child-one/body-1.jsonl"
    )));
    assert!(!inventory.keys().any(|p| p.components().any(|part| {
        [
            "private",
            ".venv",
            "node_modules",
            "attachments",
            "tool-images",
        ]
        .iter()
        .any(|name| part.as_os_str() == *name)
    })));
    for name in ["private", "attachments", "tool-images"] {
        assert!(!saved.join("data/sessions/session-one").join(name).exists());
    }
    let copied = updates
        .iter()
        .filter_map(|(stage, progress)| {
            (*stage == RuntimeHostStartupStage::HistoryBackup)
                .then_some(*progress)
                .flatten()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        copied
            .iter()
            .map(|v| (v.completed, v.total))
            .collect::<Vec<_>>(),
        vec![(0, 2), (1, 2), (2, 2)]
    );
    assert!(
        updates
            .iter()
            .any(|(stage, _)| *stage == RuntimeHostStartupStage::BackupVerification)
    );
    assert_eq!(
        fs::read(child.join("private/.venv/body-1.jsonl")).unwrap(),
        b"excluded dependency"
    );
    assert_eq!(
        source
            .query_row("SELECT COUNT(*) FROM fixture", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn deletion_recovery_staging_only_backs_up_history_not_the_project() {
    let home = home();
    let (path, source) = fixture(home.path());
    let session = home
        .path()
        .join("data/staging/deletions/session-two.pending");
    fs::create_dir_all(session.join("private/.venv")).unwrap();
    fs::write(session.join("conversation.3.jsonl"), b"staged history").unwrap();
    fs::write(session.join("private/.venv/important.py"), b"project").unwrap();
    let saved = create(&source, home.path(), &path, "0.32.1").unwrap();
    let staged = saved.join("data/staging/deletions/session-two.pending");
    assert_eq!(
        fs::read(staged.join("conversation.3.jsonl")).unwrap(),
        b"staged history"
    );
    assert!(!staged.join("private").exists());
}

#[test]
fn upgrade_cleanup_rejects_directories_outside_its_backup_root() {
    let home = home();
    let outside = home.path().join("unrelated");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("preserve"), b"unrelated data").unwrap();
    assert!(matches!(
        remove_after_upgrade(home.path(), &outside),
        Err(MigrationError::InvalidPath)
    ));
    assert_eq!(
        fs::read(outside.join("preserve")).unwrap(),
        b"unrelated data"
    );
    #[cfg(unix)]
    {
        let root = home.path().join("backups/database");
        fs::create_dir_all(&root).unwrap();
        let link = root.join("upgrade-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(matches!(
            remove_after_upgrade(home.path(), &link),
            Err(MigrationError::InvalidPath)
        ));
        assert_eq!(
            fs::read(outside.join("preserve")).unwrap(),
            b"unrelated data"
        );
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }
}
