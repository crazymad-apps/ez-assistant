//! 设备级桌面展示偏好；不承载 Runtime 业务状态。

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager as _};
use thiserror::Error;

const PREFERENCES_FILE: &str = "desktop-preferences.json";
const PREFERENCES_STAGING_FILE: &str = ".desktop-preferences.tmp";
const MAX_EXPANDED_WORKSPACES: usize = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DesktopPreferences {
    left_sidebar_open: bool,
    right_sidebar_open: bool,
    #[serde(default)]
    expanded_workspace_ids: Option<Vec<String>>,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            left_sidebar_open: true,
            right_sidebar_open: true,
            expanded_workspace_ids: None,
        }
    }
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
) -> Result<DesktopPreferences, DesktopPreferencesError> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| DesktopPreferencesError::PathUnavailable)?;
    load_from_directory(&directory)
}

#[tauri::command]
pub(crate) fn save_desktop_preferences(
    app: AppHandle,
    preferences: DesktopPreferences,
) -> Result<(), DesktopPreferencesError> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| DesktopPreferencesError::PathUnavailable)?;
    save_to_directory(&directory, preferences)
}

fn load_from_directory(directory: &Path) -> Result<DesktopPreferences, DesktopPreferencesError> {
    let path = directory.join(PREFERENCES_FILE);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DesktopPreferences::default());
        }
        Err(_) => return Err(DesktopPreferencesError::Invalid),
    };
    let preferences: DesktopPreferences =
        serde_json::from_slice(&bytes).map_err(|_| DesktopPreferencesError::Invalid)?;
    validate(preferences)
}

fn save_to_directory(
    directory: &Path,
    preferences: DesktopPreferences,
) -> Result<(), DesktopPreferencesError> {
    let preferences = validate(preferences)?;
    fs::create_dir_all(directory).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let staging = directory.join(PREFERENCES_STAGING_FILE);
    let destination = directory.join(PREFERENCES_FILE);
    let bytes =
        serde_json::to_vec_pretty(&preferences).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    fs::write(&staging, bytes).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    fs::rename(&staging, destination).map_err(|_| DesktopPreferencesError::SaveFailed)
}

fn validate(
    mut preferences: DesktopPreferences,
) -> Result<DesktopPreferences, DesktopPreferencesError> {
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn preferences_round_trip_without_runtime_state() {
        let directory = tempdir().expect("tempdir");
        let preferences = DesktopPreferences {
            left_sidebar_open: false,
            right_sidebar_open: true,
            expanded_workspace_ids: Some(vec!["workspace-b".to_owned(), "workspace-a".to_owned()]),
        };
        save_to_directory(directory.path(), preferences).expect("save");
        let loaded = load_from_directory(directory.path()).expect("load");

        assert!(!loaded.left_sidebar_open);
        assert!(loaded.right_sidebar_open);
        assert_eq!(
            loaded
                .expanded_workspace_ids
                .expect("explicit expansion state"),
            ["workspace-a", "workspace-b"]
        );
    }
}
