use super::*;

#[test]
fn automatic_updates_never_downgrade_or_replace_equal_versions() {
    let installed: InstalledPluginSnapshot = serde_json::from_value(serde_json::json!({
        "name":"test","display_name":"Test","description":"",
        "origin":{"type":"upload"},"version":"1.2.0","sha256":"old",
        "enabled":false,"package_state":"available","settings_revision":"settings",
        "configuration":[],"external_software":[],"skills":[],"mcp":[]
    }))
    .unwrap();
    let mut selection = PluginPackageSelection {
        name: "test".into(),
        version: "1.3.0".into(),
        sha256: "new".into(),
        size_bytes: 1,
        format: assistant_protocol::PluginArchiveFormat::TarGzip,
        origin: PluginPackageOrigin::Center {
            center_id: "center".into(),
        },
        expected_active_sha256: Some("old".into()),
    };
    assert!(newer(&installed, &selection));
    for version in ["1.2.0", "1.1.9", "1.2.0-beta.1", "invalid"] {
        selection.version = version.into();
        assert!(!newer(&installed, &selection), "{version}");
    }
    selection.version = "1.3.0".into();
    selection.sha256 = "old".into();
    assert!(!newer(&installed, &selection));
    selection.sha256 = "new".into();
    let mut missing = installed;
    missing.sha256 = None;
    assert!(
        !newer(&missing, &selection),
        "不允许缺失预期摘要的更新变成首次安装"
    );
}
