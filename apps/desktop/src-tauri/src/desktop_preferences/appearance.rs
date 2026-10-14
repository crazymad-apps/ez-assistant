//! 原生外观仅投影内置配色；保存仍归父模块的同一个偏好文档。

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Deserialize;
use tauri::{Manager as _, Theme, WebviewWindowBuilder, window::Color};

use super::{DEFAULT_THEME_ID, THEME_PRESETS, load_desktop_theme};

#[derive(Deserialize)]
struct NativePreset {
    color_mode: Theme,
    startup_background: Color,
}

// 只保留创建窗口时的一次读取失败事实；首次前端读取消费它，reload 仍重新读取最新偏好。
struct StartupReadFailure(AtomicBool);

pub(super) fn take_startup_read_failure(app: &tauri::AppHandle) -> bool {
    app.try_state::<StartupReadFailure>()
        .is_some_and(|state| state.0.swap(false, Ordering::Relaxed))
}

fn preset(theme_id: &str) -> Result<NativePreset, String> {
    let mut presets: BTreeMap<String, NativePreset> =
        serde_json::from_str(THEME_PRESETS).map_err(|_| "invalid_builtin_theme")?;
    presets
        .remove(theme_id)
        .ok_or_else(|| "invalid_theme".into())
}

/// 自动创建必须先禁用；只接管 main，其他窗口以及平台合并配置保持原样。
pub(crate) fn defer_main_window(config: &mut tauri::Config) {
    for window in &mut config.app.windows {
        if window.label == "main" {
            window.create = false;
        }
    }
}

pub(crate) fn create_main_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    let loaded = load_desktop_theme(app.clone());
    app.manage(StartupReadFailure(AtomicBool::new(loaded.is_err())));
    let theme_id = loaded
        .map(|value| value.theme_id)
        .unwrap_or_else(|_| DEFAULT_THEME_ID.into());
    build_main_window(app, &theme_id)
}

pub(crate) fn build_main_window(app: &tauri::AppHandle, theme_id: &str) -> tauri::Result<()> {
    let appearance = preset(theme_id).expect("built-in native theme must be valid");
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .expect("main window configuration must exist");
    WebviewWindowBuilder::from_config(app, config)?
        .theme(Some(appearance.color_mode))
        .background_color(appearance.startup_background)
        .build()?;
    Ok(())
}

#[tauri::command]
pub(crate) fn apply_desktop_theme(app: tauri::AppHandle, theme_id: String) -> Result<(), String> {
    let appearance = preset(&theme_id)?;
    // 资源浏览器是 main Window 的子 WebView，不能依赖 get_webview_window 的单视图限制。
    let window = app.get_window("main").ok_or("main_window_unavailable")?;
    window
        .set_theme(Some(appearance.color_mode))
        .map_err(|_| "window_theme_failed")?;
    #[cfg(target_os = "macos")]
    {
        // 首屏由预设实色承接；DOM 已应用配色后恢复透明，保留现有 Sidebar 材质。
        // WKWebView 的透明性来自 from_config，平台不支持运行时 set_background_color。
        window
            .set_background_color(Some(Color(0, 0, 0, 0)))
            .map_err(|_| "window_background_failed")?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        window
            .set_background_color(Some(appearance.startup_background))
            .map_err(|_| "window_background_failed")?;
        app.get_webview("main")
            .ok_or("main_webview_unavailable")?
            .set_background_color(Some(appearance.startup_background))
            .map_err(|_| "webview_background_failed")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_have_valid_native_colors_and_explicit_modes() {
        for (id, mode, color) in [
            ("default_light", Theme::Light, Color(255, 255, 255, 255)),
            ("monokai", Theme::Dark, Color(39, 40, 34, 255)),
            ("solarized_light", Theme::Light, Color(253, 246, 227, 255)),
            ("solarized_dark", Theme::Dark, Color(0, 43, 54, 255)),
        ] {
            let value = preset(id).expect("preset");
            assert_eq!(value.color_mode, mode);
            assert_eq!(value.startup_background, color);
        }
        assert!(preset("unknown").is_err());
    }

    #[test]
    fn only_main_creation_is_deferred() {
        let mut config = tauri::Config::default();
        config.app.windows = vec![
            tauri::utils::config::WindowConfig {
                label: "main".into(),
                transparent: true,
                ..Default::default()
            },
            tauri::utils::config::WindowConfig {
                label: "other".into(),
                ..Default::default()
            },
        ];
        defer_main_window(&mut config);
        assert!(!config.app.windows[0].create);
        assert!(config.app.windows[0].transparent);
        assert!(config.app.windows[1].create);
    }
}
