use tempfile::tempdir;

use super::*;

#[test]
fn host_namespaces_preserve_local_preferences_and_do_not_accept_credential_urls() {
    let directory = tempdir().unwrap();
    let local = DesktopPreferences {
        left_sidebar_width: 300,
        last_model_selection: Some(ModelSelection {
            provider_instance_id: assistant_protocol::ProviderInstanceId::new("local-provider")
                .expect("provider id"),
            model_id: "local-model".to_owned(),
        }),
        ..DesktopPreferences::default()
    };
    save_to_directory(directory.path(), local.clone()).unwrap();
    let remote = DesktopPreferences {
        left_sidebar_width: 400,
        default_approval_mode: ApprovalMode::Auto,
        last_model_selection: Some(ModelSelection {
            provider_instance_id: assistant_protocol::ProviderInstanceId::new("remote-provider")
                .expect("provider id"),
            model_id: "remote-model".to_owned(),
        }),
        resource_workspace: Some(
            serde_json::json!({"current_scope_key":"session:remote","groups":[]}),
        ),
        ..DesktopPreferences::default()
    };
    save_namespace(
        directory.path(),
        remote.clone(),
        Some("http://host.test:7240"),
    )
    .unwrap();
    let document = load_document(directory.path()).unwrap();
    assert_eq!(document.local, local);
    assert_eq!(document.hosts.get("http://host.test:7240"), Some(&remote));
    save_to_directory(directory.path(), DesktopPreferences::default()).unwrap();
    assert_eq!(
        load_document(directory.path())
            .unwrap()
            .hosts
            .get("http://host.test:7240"),
        Some(&remote)
    );
    assert!(validate_namespace("http://user:password@host.test:7240").is_err());
    assert!(validate_namespace("http://host.test:7240/#token=x").is_err());
    assert!(validate_namespace("file:///tmp").is_err());
}

#[test]
fn account_close_preference_is_used_by_native_window_close_without_replacing_local_layout() {
    let directory = tempdir().expect("isolated desktop preferences");
    let mut local = DesktopPreferences {
        left_sidebar_width: 312,
        close_behavior: DesktopCloseBehavior::HideToTray,
        ..Default::default()
    };
    save_to_directory(directory.path(), local.clone()).unwrap();
    save_theme_to_directory(directory.path(), DEFAULT_THEME_ID).unwrap();
    let other_namespace = "http://remote.test:7240";
    save_namespace(directory.path(), local.clone(), Some(other_namespace)).unwrap();
    let account_namespace = "http://host.test:7240/users/11111111-1111-4111-8111-111111111111/7";

    for behavior in [
        DesktopCloseBehavior::QuitDesktop,
        DesktopCloseBehavior::HideToTray,
    ] {
        let account = DesktopPreferences {
            left_sidebar_width: 400,
            close_behavior: behavior,
            ..Default::default()
        };
        save_namespace(directory.path(), account.clone(), Some(account_namespace)).unwrap();
        // 原生关闭不带登录身份，必须从同一设备偏好读到当前界面已保存的选择。
        local.close_behavior = behavior;
        assert_eq!(load_from_directory(directory.path()).unwrap(), local);
        let document = load_document(directory.path()).unwrap();
        assert_eq!(document.hosts[account_namespace], account);
        assert_eq!(
            document.hosts[other_namespace].close_behavior,
            DesktopCloseBehavior::HideToTray
        );
        assert_eq!(document.theme_id, Some(serde_json::json!(DEFAULT_THEME_ID)));
    }
}

#[test]
fn preferences_round_trip_without_runtime_state() {
    let directory = tempdir().expect("tempdir");
    let preferences = DesktopPreferences {
        left_sidebar_open: false,
        right_sidebar_open: true,
        left_sidebar_width: 312,
        right_sidebar_width: 374,
        expanded_workspace_ids: Some(vec!["workspace-b".to_owned(), "workspace-a".to_owned()]),
        close_behavior: DesktopCloseBehavior::QuitDesktop,
        default_approval_mode: ApprovalMode::Auto,
        last_model_selection: Some(ModelSelection {
            provider_instance_id: assistant_protocol::ProviderInstanceId::new("provider-a")
                .expect("provider id"),
            model_id: "model-a".to_owned(),
        }),
        resource_workspace: Some(serde_json::json!({"current_scope_key":"session:a","groups":[]})),
        desktop_workspace: None,
    };
    save_to_directory(directory.path(), preferences).expect("save");
    let loaded = load_from_directory(directory.path()).expect("load");

    assert!(!loaded.left_sidebar_open);
    assert!(loaded.right_sidebar_open);
    assert_eq!(loaded.left_sidebar_width, 312);
    assert_eq!(loaded.right_sidebar_width, 374);
    assert_eq!(loaded.close_behavior, DesktopCloseBehavior::QuitDesktop);
    assert_eq!(loaded.default_approval_mode, ApprovalMode::Auto);
    assert_eq!(
        loaded.last_model_selection.expect("model").model_id,
        "model-a"
    );
    assert_eq!(
        loaded.resource_workspace.unwrap()["current_scope_key"],
        "session:a"
    );
    assert_eq!(
        loaded
            .expanded_workspace_ids
            .expect("explicit expansion state"),
        ["workspace-a", "workspace-b"]
    );
}

#[test]
fn oversize_snapshot_does_not_replace_last_successful_save() {
    let directory = tempdir().expect("tempdir");
    save_to_directory(directory.path(), DesktopPreferences::default()).expect("initial save");
    let oversized = DesktopPreferences {
        resource_workspace: Some(
            serde_json::json!({"data": "x".repeat(MAX_PREFERENCES_BYTES as usize)}),
        ),
        ..DesktopPreferences::default()
    };
    assert!(matches!(
        save_to_directory(directory.path(), oversized),
        Err(DesktopPreferencesError::Invalid)
    ));
    assert_eq!(
        load_from_directory(directory.path()).expect("previous snapshot"),
        DesktopPreferences::default()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(directory.path().join(PREFERENCES_FILE))
                .expect("private file")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn missing_widths_use_defaults_and_only_minimum_widths_are_clamped() {
    let legacy: DesktopPreferences =
        serde_json::from_str(r#"{"left_sidebar_open":true,"right_sidebar_open":false}"#)
            .expect("legacy preferences");
    assert_eq!(legacy.left_sidebar_width, LEFT_SIDEBAR_DEFAULT_WIDTH);
    assert_eq!(legacy.right_sidebar_width, RIGHT_SIDEBAR_DEFAULT_WIDTH);
    assert_eq!(legacy.default_approval_mode, ApprovalMode::Auto);
    assert!(legacy.last_model_selection.is_none());
    let explicit: DesktopPreferences = serde_json::from_str(
        r#"{"left_sidebar_open":true,"right_sidebar_open":false,"default_approval_mode":"ask"}"#,
    )
    .expect("explicit approval preference");
    assert_eq!(explicit.default_approval_mode, ApprovalMode::Ask);

    let clamped = validate(DesktopPreferences {
        left_sidebar_width: -10,
        right_sidebar_width: 9_999,
        ..DesktopPreferences::default()
    })
    .expect("clamped preferences");
    assert_eq!(clamped.left_sidebar_width, LEFT_SIDEBAR_MIN_WIDTH);
    assert_eq!(clamped.right_sidebar_width, 9_999);

    let proportional = validate(DesktopPreferences {
        right_sidebar_width: 150,
        ..DesktopPreferences::default()
    })
    .expect("proportional preference");
    assert_eq!(proportional.right_sidebar_width, 150);
}

#[test]
fn invalid_device_defaults_are_rejected_instead_of_becoming_runtime_state() {
    assert!(serde_json::from_str::<DesktopPreferences>(
        r#"{"left_sidebar_open":true,"right_sidebar_open":true,"default_approval_mode":"unknown"}"#,
    )
    .is_err());
    assert!(
        validate(DesktopPreferences {
            last_model_selection: Some(ModelSelection {
                provider_instance_id: assistant_protocol::ProviderInstanceId::new("provider-a")
                    .expect("provider id"),
                model_id: "   ".to_owned(),
            }),
            ..DesktopPreferences::default()
        })
        .is_err()
    );
}

#[test]
fn network_preferences_backup_original_once_and_preserve_other_namespaces_and_theme() {
    let directory = tempdir().unwrap();
    save_to_directory(directory.path(), DesktopPreferences::default()).unwrap();
    save_theme_to_directory(directory.path(), "default_light").unwrap();
    let original = fs::read(directory.path().join(PREFERENCES_FILE)).unwrap();
    let namespace = "http://host.test:7240/users/11111111-1111-4111-8111-111111111111/7";
    let preferences = DesktopPreferences {
        desktop_workspace: Some(
            serde_json::json!({ "version": 1, "owner": namespace, "pins": { "imported": true, "items": [] } }),
        ),
        ..DesktopPreferences::default()
    };
    save_namespace(directory.path(), preferences.clone(), Some(namespace)).unwrap();
    let backup = directory
        .path()
        .join("desktop-preferences.pre-network.json");
    assert_eq!(fs::read(&backup).unwrap(), original);
    let restored: PreferencesDocument =
        serde_json::from_slice(&fs::read(&backup).unwrap()).unwrap();
    assert!(restored.hosts.is_empty());
    save_namespace(directory.path(), preferences.clone(), Some(namespace)).unwrap();
    let document = load_document(directory.path()).unwrap();
    assert_eq!(document.hosts.get(namespace), Some(&preferences));
    assert_eq!(document.local, DesktopPreferences::default());
    assert_eq!(document.theme_id, Some(serde_json::json!("default_light")));
    assert_eq!(fs::read(&backup).unwrap(), original);
}

#[test]
fn new_and_missing_close_preferences_default_to_quit_but_existing_values_survive() {
    let directory = tempdir().expect("new isolated preferences");
    assert_eq!(
        load_from_directory(directory.path())
            .unwrap()
            .close_behavior,
        DesktopCloseBehavior::QuitDesktop
    );
    let path = directory.path().join(PREFERENCES_FILE);
    fs::write(
        &path,
        r#"{"left_sidebar_open":true,"right_sidebar_open":true}"#,
    )
    .unwrap();
    assert_eq!(
        load_from_directory(directory.path())
            .unwrap()
            .close_behavior,
        DesktopCloseBehavior::QuitDesktop
    );
    for behavior in [
        DesktopCloseBehavior::HideToTray,
        DesktopCloseBehavior::QuitDesktop,
    ] {
        let preferences = DesktopPreferences {
            close_behavior: behavior,
            ..Default::default()
        };
        save_to_directory(directory.path(), preferences.clone()).unwrap();
        save_namespace(
            directory.path(),
            preferences,
            Some("http://remote.test:7240"),
        )
        .unwrap();
        save_theme_to_directory(directory.path(), DEFAULT_THEME_ID).unwrap();
        let document = load_document(directory.path()).unwrap();
        assert_eq!(document.local.close_behavior, behavior);
        assert_eq!(
            document.hosts["http://remote.test:7240"].close_behavior,
            behavior
        );
    }
}
