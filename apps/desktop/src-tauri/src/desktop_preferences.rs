//! 设备级桌面展示偏好；不承载 Runtime 业务状态。

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager as _};
use thiserror::Error;

const PREFERENCES_FILE: &str = "desktop-preferences.json";
const PREFERENCES_STAGING_FILE: &str = ".desktop-preferences.tmp";
const MAX_EXPANDED_WORKSPACES: usize = 256;
const LEFT_SIDEBAR_DEFAULT_WIDTH: i32 = 286;
const LEFT_SIDEBAR_MIN_WIDTH: i32 = 220;
const LEFT_SIDEBAR_MAX_WIDTH: i32 = 420;
const RIGHT_SIDEBAR_DEFAULT_WIDTH: i32 = 326;
const RIGHT_SIDEBAR_MIN_WIDTH: i32 = 220;
const RIGHT_SIDEBAR_MAX_WIDTH: i32 = 800;

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
    preferences.left_sidebar_width = preferences
        .left_sidebar_width
        .clamp(LEFT_SIDEBAR_MIN_WIDTH, LEFT_SIDEBAR_MAX_WIDTH);
    preferences.right_sidebar_width = preferences
        .right_sidebar_width
        .clamp(RIGHT_SIDEBAR_MIN_WIDTH, RIGHT_SIDEBAR_MAX_WIDTH);
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
    fn preferences_round_trip_without_runtime_state() {
        let directory = tempdir().expect("tempdir");
        let preferences = DesktopPreferences {
            left_sidebar_open: false,
            right_sidebar_open: true,
            left_sidebar_width: 312,
            right_sidebar_width: 374,
            expanded_workspace_ids: Some(vec!["workspace-b".to_owned(), "workspace-a".to_owned()]),
            close_behavior: DesktopCloseBehavior::QuitDesktop,
        };
        save_to_directory(directory.path(), preferences).expect("save");
        let loaded = load_from_directory(directory.path()).expect("load");

        assert!(!loaded.left_sidebar_open);
        assert!(loaded.right_sidebar_open);
        assert_eq!(loaded.left_sidebar_width, 312);
        assert_eq!(loaded.right_sidebar_width, 374);
        assert_eq!(loaded.close_behavior, DesktopCloseBehavior::QuitDesktop);
        assert_eq!(
            loaded
                .expanded_workspace_ids
                .expect("explicit expansion state"),
            ["workspace-a", "workspace-b"]
        );
    }

    #[test]
    fn missing_widths_use_defaults_and_out_of_range_widths_are_clamped() {
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
        assert_eq!(clamped.right_sidebar_width, RIGHT_SIDEBAR_MAX_WIDTH);
    }
}
