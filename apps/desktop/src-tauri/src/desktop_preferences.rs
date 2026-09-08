//! 设备级桌面展示偏好；不承载 Runtime 业务状态。

use std::{collections::BTreeMap, fs, path::Path, sync::Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager as _};
use thiserror::Error;

static PREFERENCES_WRITE: Mutex<()> = Mutex::new(());

const PREFERENCES_FILE: &str = "desktop-preferences.json";
const PREFERENCES_STAGING_FILE: &str = ".desktop-preferences.tmp";
const MAX_EXPANDED_WORKSPACES: usize = 256;
const MAX_PREFERENCES_BYTES: u64 = 4 * 1024 * 1024;
const LEFT_SIDEBAR_DEFAULT_WIDTH: i32 = 286;
const LEFT_SIDEBAR_MIN_WIDTH: i32 = 220;
const LEFT_SIDEBAR_MAX_WIDTH: i32 = 420;
const RIGHT_SIDEBAR_DEFAULT_WIDTH: i32 = 380;
const RIGHT_SIDEBAR_MIN_WIDTH: i32 = 320;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DesktopPreferences {
    left_sidebar_open: bool,
    right_sidebar_open: bool,
    #[serde(default = "default_left_sidebar_width")]
    left_sidebar_width: i32,
    #[serde(default = "default_right_sidebar_width")]
    right_sidebar_width: i32,
    #[serde(default)]
    expanded_workspace_ids: Option<Vec<String>>,
    #[serde(default)]
    close_behavior: DesktopCloseBehavior,
    /// WebView 拥有的轻量恢复索引；原生层只限界并原子保存，不装配 Runtime 业务状态。
    #[serde(default)]
    resource_workspace: Option<serde_json::Value>,
}

#[derive(Default, Deserialize, Serialize)]
struct PreferencesDocument {
    #[serde(flatten)]
    local: DesktopPreferences,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    hosts: BTreeMap<String, DesktopPreferences>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopCloseBehavior {
    #[default]
    HideToTray,
    QuitDesktop,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            left_sidebar_open: true,
            right_sidebar_open: true,
            left_sidebar_width: LEFT_SIDEBAR_DEFAULT_WIDTH,
            right_sidebar_width: RIGHT_SIDEBAR_DEFAULT_WIDTH,
            expanded_workspace_ids: None,
            close_behavior: DesktopCloseBehavior::HideToTray,
            resource_workspace: None,
        }
    }
}

pub(crate) fn load_close_behavior<R: tauri::Runtime>(app: &AppHandle<R>) -> DesktopCloseBehavior {
    let Ok(directory) = app.path().app_config_dir() else {
        return DesktopCloseBehavior::default();
    };
    load_from_directory(&directory)
        .map(|preferences| preferences.close_behavior)
        .unwrap_or_default()
}

#[derive(Debug, Error, Serialize)]
#[error("desktop preferences are unavailable")]
pub(crate) enum DesktopPreferencesError {
    #[error("desktop preferences path is unavailable")]
    PathUnavailable,
    #[error("desktop preferences are invalid")]
    Invalid,
    #[error("desktop preferences could not be saved")]
    SaveFailed,
}

#[tauri::command]
pub(crate) fn load_desktop_preferences(
    app: AppHandle,
    namespace: Option<String>,
) -> Result<DesktopPreferences, DesktopPreferencesError> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| DesktopPreferencesError::PathUnavailable)?;
    match namespace {
        None => load_from_directory(&directory),
        Some(namespace) => {
            validate_namespace(&namespace)?;
            Ok(load_document(&directory)?
                .hosts
                .remove(&namespace)
                .unwrap_or_default())
        }
    }
}

#[tauri::command]
pub(crate) fn save_desktop_preferences(
    app: AppHandle,
    preferences: DesktopPreferences,
    namespace: Option<String>,
) -> Result<(), DesktopPreferencesError> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| DesktopPreferencesError::PathUnavailable)?;
    save_namespace(&directory, preferences, namespace.as_deref())
}

fn load_from_directory(directory: &Path) -> Result<DesktopPreferences, DesktopPreferencesError> {
    Ok(load_document(directory)?.local)
}

fn load_document(directory: &Path) -> Result<PreferencesDocument, DesktopPreferencesError> {
    let path = directory.join(PREFERENCES_FILE);
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_PREFERENCES_BYTES) {
        return Err(DesktopPreferencesError::Invalid);
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreferencesDocument::default());
        }
        Err(_) => return Err(DesktopPreferencesError::Invalid),
    };
    let mut document: PreferencesDocument =
        serde_json::from_slice(&bytes).map_err(|_| DesktopPreferencesError::Invalid)?;
    document.local = validate(document.local)?;
    if document.hosts.len() > 64 {
        return Err(DesktopPreferencesError::Invalid);
    }
    for (namespace, preferences) in &mut document.hosts {
        validate_namespace(namespace)?;
        *preferences = validate(preferences.clone())?;
    }
    Ok(document)
}

#[cfg(test)]
fn save_to_directory(
    directory: &Path,
    preferences: DesktopPreferences,
) -> Result<(), DesktopPreferencesError> {
    save_namespace(directory, preferences, None)
}

fn validate_namespace(value: &str) -> Result<(), DesktopPreferencesError> {
    let url = reqwest::Url::parse(value).map_err(|_| DesktopPreferencesError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") || url.origin().ascii_serialization() != value {
        return Err(DesktopPreferencesError::Invalid);
    }
    Ok(())
}
fn save_namespace(
    directory: &Path,
    preferences: DesktopPreferences,
    namespace: Option<&str>,
) -> Result<(), DesktopPreferencesError> {
    let _guard = PREFERENCES_WRITE
        .lock()
        .map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let preferences = validate(preferences)?;
    let mut document = load_document(directory)?;
    if let Some(namespace) = namespace {
        validate_namespace(namespace)?;
        if document.hosts.len() >= 64 && !document.hosts.contains_key(namespace) {
            return Err(DesktopPreferencesError::Invalid);
        }
        document.hosts.insert(namespace.to_owned(), preferences);
    } else {
        document.local = preferences;
    }
    fs::create_dir_all(directory).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let staging = directory.join(PREFERENCES_STAGING_FILE);
    let destination = directory.join(PREFERENCES_FILE);
    let bytes =
        serde_json::to_vec_pretty(&document).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err(DesktopPreferencesError::Invalid);
    }
    // 快照含用户路径及网址，暂存文件使用私有权限，写完后再替换上一次成功快照。
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options
        .open(&staging)
        .map_err(|_| DesktopPreferencesError::SaveFailed)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| DesktopPreferencesError::SaveFailed)?;
    fs::rename(&staging, destination).map_err(|_| DesktopPreferencesError::SaveFailed)
}

fn validate(
    mut preferences: DesktopPreferences,
) -> Result<DesktopPreferences, DesktopPreferencesError> {
    if preferences
        .resource_workspace
        .as_ref()
        .is_some_and(|snapshot| !snapshot.is_object())
    {
        return Err(DesktopPreferencesError::Invalid);
    }
    preferences.left_sidebar_width = preferences
        .left_sidebar_width
        .clamp(LEFT_SIDEBAR_MIN_WIDTH, LEFT_SIDEBAR_MAX_WIDTH);
    preferences.right_sidebar_width = preferences.right_sidebar_width.max(RIGHT_SIDEBAR_MIN_WIDTH);
    if let Some(expanded_workspace_ids) = &mut preferences.expanded_workspace_ids {
        if expanded_workspace_ids.len() > MAX_EXPANDED_WORKSPACES
            || expanded_workspace_ids
                .iter()
                .any(|id| id.trim().is_empty() || id.len() > 256)
        {
            return Err(DesktopPreferencesError::Invalid);
        }
        expanded_workspace_ids.sort();
        expanded_workspace_ids.dedup();
    }
    Ok(preferences)
}

const fn default_left_sidebar_width() -> i32 {
    LEFT_SIDEBAR_DEFAULT_WIDTH
}

const fn default_right_sidebar_width() -> i32 {
    RIGHT_SIDEBAR_DEFAULT_WIDTH
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn host_namespaces_preserve_local_preferences_and_do_not_accept_credential_urls() {
        let directory = tempdir().unwrap();
        let local = DesktopPreferences {
            left_sidebar_width: 300,
            ..DesktopPreferences::default()
        };
        save_to_directory(directory.path(), local.clone()).unwrap();
        let remote = DesktopPreferences {
            left_sidebar_width: 400,
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
    fn preferences_round_trip_without_runtime_state() {
        let directory = tempdir().expect("tempdir");
        let preferences = DesktopPreferences {
            left_sidebar_open: false,
            right_sidebar_open: true,
            left_sidebar_width: 312,
            right_sidebar_width: 374,
            expanded_workspace_ids: Some(vec!["workspace-b".to_owned(), "workspace-a".to_owned()]),
            close_behavior: DesktopCloseBehavior::QuitDesktop,
            resource_workspace: Some(
                serde_json::json!({"current_scope_key":"session:a","groups":[]}),
            ),
        };
        save_to_directory(directory.path(), preferences).expect("save");
        let loaded = load_from_directory(directory.path()).expect("load");

        assert!(!loaded.left_sidebar_open);
        assert!(loaded.right_sidebar_open);
        assert_eq!(loaded.left_sidebar_width, 312);
        assert_eq!(loaded.right_sidebar_width, 374);
        assert_eq!(loaded.close_behavior, DesktopCloseBehavior::QuitDesktop);
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

        let clamped = validate(DesktopPreferences {
            left_sidebar_width: -10,
            right_sidebar_width: 9_999,
            ..DesktopPreferences::default()
        })
        .expect("clamped preferences");
        assert_eq!(clamped.left_sidebar_width, LEFT_SIDEBAR_MIN_WIDTH);
        assert_eq!(clamped.right_sidebar_width, 9_999);
    }
}
