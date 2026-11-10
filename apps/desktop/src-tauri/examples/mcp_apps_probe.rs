//! 显式运行的 MCP Apps 原生 CSP 探针；只读取新建隔离场景中的通用页面资产，不装配 Runtime。

use std::{borrow::Cow, collections::BTreeMap, path::PathBuf, time::Duration};
use tauri::{
    Manager as _, WebviewUrl,
    utils::assets::{AssetKey, AssetsIter, CspHash},
};

struct ProbeAssets(BTreeMap<String, Vec<u8>>);

impl<R: tauri::Runtime> tauri::Assets<R> for ProbeAssets {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        self.0
            .get(key.as_ref())
            .map(|bytes| Cow::Borrowed(bytes.as_slice()))
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(
            self.0
                .iter()
                .map(|(key, bytes)| (Cow::Borrowed(key.as_str()), Cow::Borrowed(bytes.as_slice()))),
        )
    }

    fn csp_hashes(&self, _html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

#[tauri::command]
fn probe_ping() -> bool {
    true
}

#[tauri::command]
fn probe_finish(app: tauri::AppHandle, passed: bool, summary: String) {
    println!("{}: {summary}", if passed { "PASS" } else { "FAIL" });
    app.exit(i32::from(!passed));
}

fn main() {
    let directory = PathBuf::from(std::env::args().nth(1).expect("new isolated scene path"))
        .canonicalize()
        .expect("scene exists");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.runtime-test")
        .canonicalize()
        .expect("test root");
    assert!(
        directory.starts_with(&root) && directory != root,
        "only isolated scenes are allowed"
    );
    let mut assets = BTreeMap::new();
    for name in ["probe.html", "probe.js", "resource.json", "style.css"] {
        assets.insert(
            format!("/{name}"),
            std::fs::read(directory.join(name)).expect("prepared fixture asset"),
        );
    }
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = format!(
        "com.ez-assistant.mcp-apps-probe.{}",
        directory.file_name().unwrap().to_string_lossy()
    );
    context.config_mut().build.dev_url = None;
    context.config_mut().app.windows.clear();
    // 使用产品原始 security.csp 和 Tauri 的正式资产协议；不改成宽松 HTTP 页面。
    context.set_assets(Box::new(ProbeAssets(assets)));
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![probe_ping, probe_finish])
        .setup(|app| {
            tauri::WebviewWindowBuilder::new(app, "main", WebviewUrl::App("probe.html".into()))
                .incognito(true)
                .title("F03 isolated MCP Apps probe")
                .inner_size(800.0, 600.0)
                .initialization_script("window.__EZ_F03_MAIN_FRAME_PROBE__ = true;")
                .initialization_script_for_all_frames("window.__EZ_F03_ALL_FRAME_PROBE__ = true;")
                .build()?;
            let handle = app.handle().clone();
            // 由该短期应用拥有的有界超时；应用正常退出时任务一并销毁。
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                eprintln!("FAIL: native MCP Apps probe timeout");
                handle.exit(1);
            });
            Ok(())
        })
        .build(context)
        .expect("build native probe");
    let config_directory = app.path().app_config_dir().expect("probe config path");
    let code = app.run_return(move |_, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let _ = std::fs::remove_dir(&config_directory);
        }
    });
    std::process::exit(code);
}
