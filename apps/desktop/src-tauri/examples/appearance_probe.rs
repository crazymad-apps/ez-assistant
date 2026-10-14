//! 显式运行的原生配色探针：不装配 Host，偏好与窗口状态仅使用新的 .runtime-test 场景。
#![allow(dead_code)]

#[path = "../src/desktop_preferences.rs"]
mod desktop_preferences;

use std::{path::PathBuf, time::Duration};
use tauri::{Manager as _, Theme};
use tauri_plugin_window_state::{AppHandleExt as _, StateFlags};

struct Probe {
    directory: PathBuf,
    second: bool,
}

#[tauri::command]
fn probe_load(
    probe: tauri::State<'_, Probe>,
) -> Result<desktop_preferences::DesktopThemePreference, String> {
    desktop_preferences::load_theme_from_directory(&probe.directory).map_err(|e| e.to_string())
}

#[tauri::command]
fn probe_save(probe: tauri::State<'_, Probe>, theme_id: String) -> Result<(), String> {
    desktop_preferences::save_theme_to_directory(&probe.directory, &theme_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn probe_round(app: tauri::AppHandle, probe: tauri::State<'_, Probe>) -> Result<bool, String> {
    if probe.second {
        let window = app.get_window("main").ok_or("missing main")?;
        let size = window
            .inner_size()
            .map_err(|e| e.to_string())?
            .to_logical::<u32>(window.scale_factor().map_err(|e| e.to_string())?);
        if size.width != 1080 || size.height.abs_diff(760) > 1 {
            return Err(format!("window size was not restored: {size:?}"));
        }
        let state: serde_json::Value = serde_json::from_slice(
            &std::fs::read(probe.directory.join("window-state.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let position = window.outer_position().map_err(|e| e.to_string())?;
        let x = state["main"]["x"].as_i64().ok_or("missing saved x")?;
        let y = state["main"]["y"].as_i64().ok_or("missing saved y")?;
        if i64::from(position.x).abs_diff(x) > 2 || i64::from(position.y).abs_diff(y) > 2 {
            return Err(format!("window position was not restored: {position:?}"));
        }
        println!("PASS: saved window size/position restored through from_config");
    }
    Ok(probe.second)
}

#[tauri::command]
fn probe_fail(app: tauri::AppHandle, message: String) {
    eprintln!("FAIL: {message}");
    app.exit(1);
}

#[tauri::command]
fn probe_finish(app: tauri::AppHandle, theme_id: String) -> Result<(), String> {
    let saved = probe_load(app.state())?;
    if saved.theme_id != theme_id {
        return Err("saved theme differs from final selection".into());
    }
    let window = app.get_window("main").ok_or("missing main")?;
    if window.theme().map_err(|e| e.to_string())? != Theme::Dark {
        return Err("native window did not adopt explicit dark mode".into());
    }
    window.hide().map_err(|e| e.to_string())?;
    window.show().map_err(|e| e.to_string())?;
    app.save_window_state(StateFlags::all())
        .map_err(|e| e.to_string())?;
    println!("PASS: DOM/native modes, saved theme, hide/show, window-state save");
    app.exit(0);
    Ok(())
}

fn main() {
    let directory = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("new .runtime-test scenario directory"),
    )
    .canonicalize()
    .expect("scenario must exist");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.runtime-test")
        .canonicalize()
        .expect("test root");
    assert!(directory.starts_with(&root) && directory != root);
    let second = std::env::args().nth(2).as_deref() == Some("second");
    if !second {
        assert!(
            !directory.join("desktop-preferences.json").exists(),
            "use a new scenario"
        );
        desktop_preferences::save_theme_to_directory(&directory, "monokai").expect("seed theme");
    }
    let saved =
        desktop_preferences::load_theme_from_directory(&directory).expect("saved preference");
    let mut context = tauri::generate_context!();
    // 唯一应用身份；WebView 非持久，不与用户的开发／正式客户端共享数据。
    context.config_mut().identifier = format!(
        "com.ez-assistant.appearance-probe.{}",
        directory.file_name().unwrap().to_string_lossy()
    );
    context.config_mut().build.dev_url = Some("http://127.0.0.1:1437".parse().unwrap());
    let main = context
        .config_mut()
        .app
        .windows
        .iter_mut()
        .find(|w| w.label == "main")
        .expect("main config");
    main.url = tauri::WebviewUrl::App("tests/native/appearance.html".into());
    main.title = "F04 Appearance Probe".into();
    main.incognito = true;
    desktop_preferences::appearance::defer_main_window(context.config_mut());
    let window_state = directory.join("window-state.json");
    let app = tauri::Builder::default()
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filename(window_state.to_string_lossy())
                .build(),
        )
        .manage(Probe { directory, second })
        .invoke_handler(tauri::generate_handler![
            probe_load,
            probe_save,
            probe_round,
            probe_fail,
            probe_finish,
            desktop_preferences::appearance::apply_desktop_theme
        ])
        .setup(move |app| {
            desktop_preferences::appearance::build_main_window(app.handle(), &saved.theme_id)?;
            let window = app.get_window("main").expect("main window");
            assert_eq!(window.theme()?, Theme::Dark, "native startup mode");
            if !second {
                window.set_size(tauri::LogicalSize::new(1080, 760))?;
            }
            println!(
                "PASS: pre-page native startup mode is dark ({})",
                saved.theme_id
            );
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(45)).await;
                eprintln!("FAIL: probe timed out");
                handle.exit(1);
            });
            Ok(())
        })
        .build(context)
        .expect("build probe");
    // 插件仅在这个独立目录创建空父目录，实际状态文件路径在 .runtime-test。
    let empty_config_directory = app.path().app_config_dir().expect("probe config directory");
    let code = app.run_return(move |_, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let _ = std::fs::remove_dir(&empty_config_directory);
        }
    });
    std::process::exit(code);
}
