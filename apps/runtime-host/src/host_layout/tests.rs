use super::*;

#[test]
fn invalid_access_settings_do_not_move_personal_files() {
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.toml"),
        "schema_version=1\n[host_access]\nport=0\n",
    )
    .unwrap();
    fs::create_dir(home.path().join("skills")).unwrap();
    assert!(matches!(upgrade(home.path()), Err(Error::Configuration)));
    assert!(home.path().join("skills").exists());
    assert!(!home.path().join("host.toml").exists());
    assert!(!personal_home(home.path()).exists());
}

#[test]
fn conflicts_and_modified_backup_stop_before_any_move() {
    for damage in ["target", "backup", "source", "multiple"] {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("config.toml"), "schema_version=1\n").unwrap();
        fs::create_dir(home.path().join("skills")).unwrap();
        fs::write(home.path().join("skills/private.md"), "private").unwrap();
        let root = home.path().join("backups/host-layout");
        let backup = create_backup(home.path(), &root).unwrap();
        match damage {
            "target" => {
                fs::create_dir_all(personal_home(home.path()).join("skills")).unwrap();
            }
            "backup" => {
                fs::write(backup.join("contents/skills/private.md"), "changed").unwrap();
            }
            "source" => {
                fs::write(home.path().join("skills/private.md"), "changed").unwrap();
            }
            "multiple" => {
                create_backup(home.path(), &root).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(upgrade(home.path()).is_err(), "{damage}");
        assert!(home.path().join("skills/private.md").is_file());
        assert!(home.path().join("config.toml").is_file());
        assert!(!home.path().join("host.toml").exists());
    }
}

#[cfg(unix)]
#[test]
fn backup_preserves_symlink_targets_without_copying_external_contents() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("private"), "external").unwrap();
    fs::create_dir(home.path().join("skills")).unwrap();
    std::os::unix::fs::symlink(outside.path(), home.path().join("skills/external")).unwrap();
    upgrade(home.path()).unwrap();
    assert_eq!(
        fs::read_link(personal_home(home.path()).join("skills/external")).unwrap(),
        outside.path()
    );
    let backup = fs::read_dir(home.path().join("backups/host-layout"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        fs::symlink_metadata(backup.join("contents/skills/external"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("private")).unwrap(),
        "external"
    );
}

#[test]
fn database_upgrade_preserves_external_paths_and_verified_backup() {
    let home = tempfile::tempdir().unwrap();
    database::legacy_fixture(home.path());
    fs::write(
        home.path().join("data/workspaces/w/agent/notes.txt"),
        "unchanged history /data/workspaces",
    )
    .unwrap();
    upgrade(home.path()).unwrap();
    let target = personal_home(home.path());
    let connection = database::admit(&target.join(DATABASE)).unwrap().unwrap();
    let (count, current, external): (i64, String, String) = connection
        .query_row(
            "SELECT COUNT(*),agent_directory,user_directory FROM workspaces",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        current,
        target.join("data/workspaces/w/agent").to_str().unwrap()
    );
    assert_eq!(external, home.path().join("external").to_str().unwrap());
    assert_eq!(
        connection
            .query_row(
                "SELECT min_compatible_host_version FROM database_compatibility",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "0.27.0"
    );
    assert_eq!(
        fs::read_to_string(target.join("data/workspaces/w/agent/notes.txt")).unwrap(),
        "unchanged history /data/workspaces"
    );
    let backup = fs::read_dir(home.path().join("backups/host-layout"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(backup.join("manifest.json")).unwrap()).unwrap();
    verify_backup(&backup.join("contents"), &manifest).unwrap();
    // 显式离线恢复验证只复制到全新隔离目录，失败现场和独立原件均保留。
    let restored = tempfile::tempdir().unwrap();
    for (relative, entry) in &manifest.entries {
        files::copy_entry(
            &backup.join("contents").join(relative),
            &restored.path().join(relative),
            entry,
        )
        .unwrap();
    }
    let restored_db = database::admit(&restored.path().join(DATABASE))
        .unwrap()
        .unwrap();
    assert_eq!(
        restored_db
            .query_row("SELECT agent_directory FROM workspaces", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        home.path()
            .join("data/workspaces/w/agent")
            .to_str()
            .unwrap()
    );
    upgrade(home.path()).unwrap();
}

#[test]
fn every_committed_file_boundary_can_reenter_without_rewriting_history() {
    for stop_at in 1..=9 {
        let home = tempfile::tempdir().unwrap();
        database::legacy_fixture(home.path());
        fs::write(
            home.path().join("config.toml"),
            "schema_version=1\n[host_access]\nport=9123\n",
        )
        .unwrap();
        fs::create_dir(home.path().join("skills")).unwrap();
        fs::write(home.path().join("skills/private.md"), "personal").unwrap();
        fs::write(
            home.path().join("mcp.json"),
            serde_json::json!({"mcpServers":{"s":{"cwd":home.path().join("skills")}}}).to_string(),
        )
        .unwrap();
        let backup = create_backup(home.path(), &home.path().join("backups/host-layout")).unwrap();
        let mut writes = 0;
        let result = finish_at_boundaries(home.path(), &backup, &mut || {
            writes += 1;
            if writes == stop_at {
                Err(Error::Io(std::io::Error::other(
                    "injected write interruption",
                )))
            } else {
                Ok(())
            }
        });
        assert!(result.is_ok() || writes == stop_at);
        upgrade(home.path()).unwrap_or_else(|error| panic!("boundary {stop_at}: {error:?}"));
        assert_eq!(
            fs::read_to_string(personal_home(home.path()).join("skills/private.md")).unwrap(),
            "personal"
        );
        assert!(!home.path().join("skills").exists());
    }
}

#[test]
fn empty_home_commits_version_without_creating_a_database() {
    let home = tempfile::tempdir().unwrap();
    upgrade(home.path()).unwrap();
    assert!(!personal_home(home.path()).join(DATABASE).exists());
    let contents = fs::read_to_string(home.path().join("host.toml")).unwrap();
    assert_eq!(
        host_configuration::parse(&contents).unwrap().mode,
        host_configuration::HostMode::Personal
    );
    upgrade(home.path()).unwrap();
    assert_eq!(
        fs::read_to_string(home.path().join("host.toml")).unwrap(),
        contents
    );
}

#[test]
fn config_and_private_files_move_once_with_only_structured_paths_changed() {
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.toml"),
        "# personal\nschema_version = 1\n[host_access]\nport = 9123\n[speech]\nenabled = false\n",
    )
    .unwrap();
    fs::create_dir(home.path().join("skills")).unwrap();
    fs::write(home.path().join("skills/private.md"), "private content").unwrap();
    fs::create_dir(home.path().join("mcp")).unwrap();
    let old = home
        .path()
        .join("mcp/server")
        .to_string_lossy()
        .into_owned();
    fs::write(
        home.path().join("mcp.json"),
        serde_json::json!({"mcpServers":{"s":{"cwd":old,"command":old,"env":{"KEEP":old}}}})
            .to_string(),
    )
    .unwrap();
    fs::write(home.path().join("recall-reference.key"), "unchanged key").unwrap();
    upgrade(home.path()).unwrap();
    let target = personal_home(home.path());
    assert!(!home.path().join("skills").exists());
    assert!(!home.path().join("config.toml").exists());
    assert_eq!(
        fs::read_to_string(target.join("recall-reference.key")).unwrap(),
        "unchanged key"
    );
    let user = fs::read_to_string(target.join("config.toml")).unwrap();
    assert!(user.contains("# personal"));
    assert!(!user.contains("host_access"));
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(target.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        json["mcpServers"]["s"]["cwd"],
        target.join("mcp/server").to_str().unwrap()
    );
    assert_eq!(json["mcpServers"]["s"]["command"], old);
    upgrade(home.path()).unwrap();
}

#[test]
fn missing_database_conflicts_and_future_versions_never_create_replacement_data() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("data/sessions/s")).unwrap();
    fs::write(home.path().join("data/sessions/s/body.jsonl"), "history").unwrap();
    assert!(matches!(upgrade(home.path()), Err(Error::MissingDatabase)));
    assert!(!home.path().join("host.toml").exists());
    fs::write(
        home.path().join("host.toml"),
        "version='99.0.0'\nmode='personal'\n",
    )
    .unwrap();
    assert!(matches!(upgrade(home.path()), Err(Error::Configuration)));
    assert!(!personal_home(home.path()).exists());
}
