//! 设备级桌面展示偏好；不承载 Runtime 业务状态。

use std::{collections::BTreeMap, fs, path::Path, sync::Mutex};

use assistant_protocol::{ApprovalMode, ModelSelection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager as _};
use thiserror::Error;

#[path = "desktop_preferences/appearance.rs"]
pub(crate) mod appearance;

static PREFERENCES_WRITE: Mutex<()> = Mutex::new(());

const PREFERENCES_FILE: &str = "desktop-preferences.json";
const PREFERENCES_STAGING_FILE: &str = ".desktop-preferences.tmp";
const THEME_PRESETS: &str = include_str!("../../theme-presets.json");
const DEFAULT_THEME_ID: &str = "default_light";
const MAX_EXPANDED_WORKSPACES: usize = 256;
const MAX_PREFERENCES_BYTES: u64 = 4 * 1024 * 1024;
const LEFT_SIDEBAR_DEFAULT_WIDTH: i32 = 286;
const LEFT_SIDEBAR_MIN_WIDTH: i32 = 220;
const LEFT_SIDEBAR_MAX_WIDTH: i32 = 420;
const RIGHT_SIDEBAR_DEFAULT_WIDTH: i32 = 380;
// 保存的是基准窗口下的宽度；大窗口中合法的 320px 右栏可能对应更小的偏好值。
const RIGHT_SIDEBAR_MIN_PREFERENCE: i32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DesktopPreferences {
    left_sidebar_open: bool,
    right_sidebar_open: bool,
    #[serde(default = "default_left_sidebar_width")]
    left_sidebar_width: i32,
    /// 基准窗口剩余空间中的宽度；WebView 按当前窗口空间换算实际右栏宽度。
    #[serde(default = "default_right_sidebar_width")]
    right_sidebar_width: i32,
    #[serde(default)]
    expanded_workspace_ids: Option<Vec<String>>,
    #[serde(default)]
    close_behavior: DesktopCloseBehavior,
    /// 仅作为这个 Desktop/目标的新草稿默认，不是 Runtime 当前 Session 状态。
    #[serde(default = "default_approval_mode")]
    default_approval_mode: ApprovalMode,
    /// 最近一次明确选择的二元模型身份；None 表示继续跟随 Runtime 全局默认。
    #[serde(default)]
    last_model_selection: Option<ModelSelection>,
    /// WebView 拥有的轻量恢复索引；原生层只限界并原子保存，不装配 Runtime 业务状态。
    #[serde(default)]
    resource_workspace: Option<serde_json::Value>,
    /// 当前身份的本机置顶和带设备归属的右栏描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desktop_workspace: Option<serde_json::Value>,
}

#[derive(Default, Deserialize, Serialize)]
struct PreferencesDocument {
    #[serde(flatten)]
    local: DesktopPreferences,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    hosts: BTreeMap<String, DesktopPreferences>,
    // 保留未知值直到用户明确选色；布局保存不能顺便规范化或丢弃该字段。
    #[serde(
        default,
        deserialize_with = "deserialize_present_theme",
        skip_serializing_if = "Option::is_none"
    )]
    theme_id: Option<serde_json::Value>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub(crate) struct DesktopThemePreference {
    pub(crate) theme_id: String,
    recovered_unknown: bool,
}

fn deserialize_present_theme<'de, D>(deserializer: D) -> Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // 缺字段是旧版正常默认，显式 null 则是未知值，两者不能被 Option 反序列化合并。
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopCloseBehavior {
    HideToTray,
    #[default]
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
            close_behavior: DesktopCloseBehavior::default(),
            default_approval_mode: default_approval_mode(),
            last_model_selection: None,
            resource_workspace: None,
            desktop_workspace: None,
        }
    }
}

fn default_approval_mode() -> ApprovalMode {
    ApprovalMode::Auto
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

#[tauri::command]
pub(crate) fn load_desktop_theme(
    app: AppHandle,
) -> Result<DesktopThemePreference, DesktopPreferencesError> {
    if appearance::take_startup_read_failure(&app) {
        return Err(DesktopPreferencesError::Invalid);
    }
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| DesktopPreferencesError::PathUnavailable)?;
    load_theme_from_directory(&directory)
}

#[tauri::command]
pub(crate) fn save_desktop_theme(
    app: AppHandle,
    theme_id: String,
) -> Result<(), DesktopPreferencesError> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|_| DesktopPreferencesError::PathUnavailable)?;
    save_theme_to_directory(&directory, &theme_id)
}

fn is_theme_id(value: &str) -> Result<bool, DesktopPreferencesError> {
    let presets: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(THEME_PRESETS).map_err(|_| DesktopPreferencesError::Invalid)?;
    Ok(presets.contains_key(value))
}

pub(crate) fn load_theme_from_directory(
    directory: &Path,
) -> Result<DesktopThemePreference, DesktopPreferencesError> {
    let document = load_document(directory)?;
    if let Some(value) = document
        .theme_id
        .as_ref()
        .and_then(serde_json::Value::as_str)
        && is_theme_id(value)?
    {
        return Ok(DesktopThemePreference {
            theme_id: value.to_owned(),
            recovered_unknown: false,
        });
    }
    Ok(DesktopThemePreference {
        theme_id: DEFAULT_THEME_ID.to_owned(),
        recovered_unknown: document.theme_id.is_some(),
    })
}

pub(crate) fn save_theme_to_directory(
    directory: &Path,
    theme_id: &str,
) -> Result<(), DesktopPreferencesError> {
    if !is_theme_id(theme_id)? {
        return Err(DesktopPreferencesError::Invalid);
    }
    // 与布局共用锁：锁内重读、只改主题、沿用同一暂存替换，防止两类保存互相覆盖。
    let _guard = PREFERENCES_WRITE
        .lock()
        .map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let mut document = load_document(directory)?;
    document.theme_id = Some(serde_json::Value::String(theme_id.to_owned()));
    write_document(directory, &document)
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
    // 企业视图按 Host / Center / 用户分区；只接纳固定数字 ID 路径，不接受凭据或任意路径。
    let path: Vec<_> = url.path().trim_matches('/').split('/').collect();
    let user_scope = path.len() == 3
        && path[0] == "users"
        && path[1].len() == 36
        && path[1]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        && path[2].parse::<i32>().is_ok_and(|id| id > 0);
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.origin().ascii_serialization() != value && !user_scope)
    {
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
    if preferences.desktop_workspace.is_some() {
        backup_legacy_document(directory)?;
    }
    if let Some(namespace) = namespace {
        validate_namespace(namespace)?;
        if document.hosts.len() >= 64 && !document.hosts.contains_key(namespace) {
            return Err(DesktopPreferencesError::Invalid);
        }
        // 登录后的布局按账号分区保存，原生关闭事件却没有账号上下文。把当前界面已保存的
        // 关闭选择同时交给设备入口，在同一写锁和原子替换内提交；其他本机布局和账号不变。
        document.local.close_behavior = preferences.close_behavior;
        document.hosts.insert(namespace.to_owned(), preferences);
    } else {
        document.local = preferences;
    }
    write_document(directory, &document)
}

/// 调用方须持有 PREFERENCES_WRITE，并在该锁内读取及合并文档；主题和布局共用唯一写入路径。
fn write_document(
    directory: &Path,
    document: &PreferencesDocument,
) -> Result<(), DesktopPreferencesError> {
    fs::create_dir_all(directory).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let staging = directory.join(PREFERENCES_STAGING_FILE);
    let destination = directory.join(PREFERENCES_FILE);
    let bytes =
        serde_json::to_vec_pretty(document).map_err(|_| DesktopPreferencesError::SaveFailed)?;
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

/// 首次写新版前保存字节一致、可读的旧偏好；失败时不提交新版，也不覆盖已有备份。
fn backup_legacy_document(directory: &Path) -> Result<(), DesktopPreferencesError> {
    use std::io::Write;
    let source = directory.join(PREFERENCES_FILE);
    if !source.exists() {
        return Ok(());
    }
    let backup = directory.join("desktop-preferences.pre-network.json");
    if backup.exists() {
        let bytes = fs::read(&backup).map_err(|_| DesktopPreferencesError::SaveFailed)?;
        serde_json::from_slice::<PreferencesDocument>(&bytes)
            .map_err(|_| DesktopPreferencesError::Invalid)?;
        return Ok(());
    }
    let bytes = fs::read(&source).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let staging = directory.join(".desktop-preferences.backup.tmp");
    let mut file = options
        .open(&staging)
        .map_err(|_| DesktopPreferencesError::SaveFailed)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| DesktopPreferencesError::SaveFailed)?;
    let saved = fs::read(&staging).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    if bytes != saved {
        return Err(DesktopPreferencesError::SaveFailed);
    }
    serde_json::from_slice::<PreferencesDocument>(&saved)
        .map_err(|_| DesktopPreferencesError::Invalid)?;
    fs::rename(&staging, &backup).map_err(|_| DesktopPreferencesError::SaveFailed)?;
    Ok(())
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
    if preferences
        .desktop_workspace
        .as_ref()
        .is_some_and(|value| !value.is_object())
    {
        return Err(DesktopPreferencesError::Invalid);
    }
    preferences.left_sidebar_width = preferences
        .left_sidebar_width
        .clamp(LEFT_SIDEBAR_MIN_WIDTH, LEFT_SIDEBAR_MAX_WIDTH);
    preferences.right_sidebar_width = preferences
        .right_sidebar_width
        .max(RIGHT_SIDEBAR_MIN_PREFERENCE);
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
    if preferences
        .last_model_selection
        .as_ref()
        .is_some_and(|selection| {
            let provider = selection.provider_instance_id.as_str();
            provider.trim().is_empty()
                || provider.len() > 256
                || selection.model_id.trim().is_empty()
                || selection.model_id.len() > 256
                || selection.model_id.chars().any(char::is_control)
        })
    {
        return Err(DesktopPreferencesError::Invalid);
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
#[path = "desktop_preferences/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "desktop_preferences/theme_tests.rs"]
mod theme_tests;
