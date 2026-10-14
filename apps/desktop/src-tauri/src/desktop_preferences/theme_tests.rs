use super::*;

fn directory() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("f04-preferences-")
        .tempdir_in(root)
        .unwrap()
}

#[test]
fn theme_missing_file_or_field_uses_default_without_creating_configuration() {
    let directory = directory();
    assert_eq!(
        load_theme_from_directory(directory.path()).unwrap(),
        DesktopThemePreference {
            theme_id: DEFAULT_THEME_ID.to_owned(),
            recovered_unknown: false,
        }
    );
    assert!(!directory.path().join(PREFERENCES_FILE).exists());
    save_to_directory(directory.path(), DesktopPreferences::default()).unwrap();
    let bytes = fs::read(directory.path().join(PREFERENCES_FILE)).unwrap();
    assert!(
        !load_theme_from_directory(directory.path())
            .unwrap()
            .recovered_unknown
    );
    assert_eq!(
        fs::read(directory.path().join(PREFERENCES_FILE)).unwrap(),
        bytes
    );
    let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(document.get("theme_id").is_none());
}

#[test]
fn theme_unknown_values_survive_layout_writes_until_explicit_selection() {
    let directory = directory();
    for value in [
        serde_json::json!("future_theme"),
        serde_json::json!(42),
        serde_json::Value::Null,
        serde_json::json!({"custom":"theme"}),
    ] {
        let mut document = serde_json::to_value(DesktopPreferences::default()).unwrap();
        document["theme_id"] = value.clone();
        fs::write(
            directory.path().join(PREFERENCES_FILE),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let theme = load_theme_from_directory(directory.path()).unwrap();
        assert_eq!(theme.theme_id, DEFAULT_THEME_ID);
        assert!(theme.recovered_unknown);
        save_to_directory(directory.path(), DesktopPreferences::default()).unwrap();
        assert_eq!(
            load_document(directory.path()).unwrap().theme_id,
            Some(value)
        );
        save_theme_to_directory(directory.path(), "monokai").unwrap();
        let theme = load_theme_from_directory(directory.path()).unwrap();
        assert_eq!(theme.theme_id, "monokai");
        assert!(!theme.recovered_unknown);
    }
}

#[test]
fn theme_and_local_and_remote_layout_writes_preserve_each_other_under_contention() {
    let directory = directory();
    let path = directory.path();
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            for theme in [
                "monokai",
                "default_light",
                "solarized_light",
                "solarized_dark",
            ] {
                save_theme_to_directory(path, theme).unwrap();
            }
        });
        scope.spawn(|| {
            barrier.wait();
            for width in 300..310 {
                save_to_directory(
                    path,
                    DesktopPreferences {
                        left_sidebar_width: width,
                        close_behavior: DesktopCloseBehavior::QuitDesktop,
                        ..DesktopPreferences::default()
                    },
                )
                .unwrap();
            }
        });
        scope.spawn(|| {
            barrier.wait();
            for width in 390..400 {
                save_namespace(
                    path,
                    DesktopPreferences {
                        left_sidebar_width: width,
                        ..DesktopPreferences::default()
                    },
                    Some("https://host.test:7240"),
                )
                .unwrap();
            }
        });
    });
    let document = load_document(path).unwrap();
    assert_eq!(document.theme_id, Some(serde_json::json!("solarized_dark")));
    assert_eq!(document.local.left_sidebar_width, 309);
    assert_eq!(
        document.local.close_behavior,
        DesktopCloseBehavior::QuitDesktop
    );
    assert_eq!(
        document.hosts["https://host.test:7240"].left_sidebar_width,
        399
    );
    // 原始文件只有一个顶层主题，身份分区没有各自的主题副本。
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(path.join(PREFERENCES_FILE)).unwrap()).unwrap();
    assert!(
        saved["hosts"]["https://host.test:7240"]
            .get("theme_id")
            .is_none()
    );
}

#[test]
fn theme_invalid_or_failed_writes_preserve_the_previous_document() {
    let directory = directory();
    let path = directory.path();
    save_theme_to_directory(path, "monokai").unwrap();
    let before = fs::read(path.join(PREFERENCES_FILE)).unwrap();
    for invalid in ["", "dark", "__proto__", "../../other"] {
        assert!(save_theme_to_directory(path, invalid).is_err());
        assert_eq!(fs::read(path.join(PREFERENCES_FILE)).unwrap(), before);
    }
    fs::create_dir(path.join(PREFERENCES_STAGING_FILE)).unwrap();
    assert!(matches!(
        save_theme_to_directory(path, "solarized_dark"),
        Err(DesktopPreferencesError::SaveFailed)
    ));
    assert_eq!(fs::read(path.join(PREFERENCES_FILE)).unwrap(), before);
}

#[test]
fn theme_damaged_document_is_not_replaced_with_defaults() {
    let directory = directory();
    let path = directory.path();
    let damaged = b"{broken preferences";
    fs::write(path.join(PREFERENCES_FILE), damaged).unwrap();
    assert!(load_theme_from_directory(path).is_err());
    assert!(save_theme_to_directory(path, "monokai").is_err());
    assert_eq!(fs::read(path.join(PREFERENCES_FILE)).unwrap(), damaged);
}

#[test]
fn theme_catalog_has_matching_native_defaults_and_valid_startup_metadata() {
    let presets: BTreeMap<String, serde_json::Value> = serde_json::from_str(THEME_PRESETS).unwrap();
    assert_eq!(presets.len(), 4);
    assert!(presets.contains_key(DEFAULT_THEME_ID));
    for (id, preset) in presets {
        assert!(is_theme_id(&id).unwrap());
        assert!(matches!(
            preset["color_mode"].as_str(),
            Some("light" | "dark")
        ));
        let color = preset["startup_background"].as_str().unwrap();
        assert!(color.starts_with('#') && color.len() == 7);
        assert!(u32::from_str_radix(&color[1..], 16).is_ok());
    }
}
