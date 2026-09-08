//! 显式按需运行的 macOS 原生网页关闭回归探针；不启动或连接 Assistant Runtime。
//! 测试页必须是自有 loopback 夹具；需要 /status 返回各页最新的播放状态。
//! cargo run -p ez-assistant-desktop --example browser_media_close -- http://127.0.0.1:PORT
#![allow(dead_code)]

#[path = "../src/browser_resource.rs"]
mod browser_resource;

use std::{
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};
use tauri::{Manager, WebviewUrl, ipc::Channel};

fn main() {
    let base = std::env::args().nth(1).expect("loopback fixture URL");
    let url = url::Url::parse(&base).expect("fixture URL");
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let raw_close = std::env::args().any(|arg| arg == "--raw-close");
    let exit_code = Arc::new(AtomicI32::new(1));
    let result_code = exit_code.clone();
    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "com.ez-assistant.browser-media-probe".into();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .manage(browser_resource::BrowserResourceManager::default())
        .setup(move |app| {
            let main = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(format!("{base}/shell").parse()?),
            )
            .title("Browser media lifecycle probe")
            .build()?;
            let caller = main.as_ref().clone();
            let app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(30),
                    verify(&app, caller, &base, raw_close),
                )
                .await
                .unwrap_or_else(|_| Err("native probe timed out".into()));
                if let Err(error) = &result {
                    eprintln!("FAIL: {error}");
                }
                let code = if result.is_ok() { 0 } else { 1 };
                result_code.store(code, Ordering::Relaxed);
                app.exit(code);
            });
            Ok(())
        })
        .run(context)
        .expect("native probe");
    std::process::exit(exit_code.load(Ordering::Relaxed));
}

async fn verify(
    app: &tauri::AppHandle,
    caller: tauri::Webview,
    base: &str,
    raw_close: bool,
) -> Result<(), String> {
    let page = format!("probe-{}", std::process::id());
    let id = browser_resource::create_resource_browser(
        caller.clone(),
        format!("{base}/?id={page}"),
        Channel::new(|_| Ok(())),
    )
    .await?;
    let view = app.get_webview(&id).ok_or("missing native browser")?;
    view.set_bounds(tauri::Rect {
        position: tauri::LogicalPosition::new(0., 0.).into(),
        size: tauri::LogicalSize::new(600., 400.).into(),
    })
    .map_err(|e| e.to_string())?;
    view.show().map_err(|e| e.to_string())?;
    // 这是可控夹具的驱动，不注入第三方站点，也不读取用户浏览数据。
    tokio::time::sleep(Duration::from_secs(2)).await;
    view.eval("document.querySelector('button').click()")
        .map_err(|e| e.to_string())?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let client = reqwest::Client::new();
    let before = status(&client, base, &page).await?;
    println!("before close: {before}");
    if before["audio"] != true || before["ctx"] != "running" {
        return Err("fixture did not start both media types".into());
    }
    browser_resource::capture_resource_browser(caller.clone(), id.clone()).await?;
    view.hide().map_err(|e| e.to_string())?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let hidden = status(&client, base, &page).await?;
    if hidden["audio"] != true
        || hidden["ctx"] != "running"
        || hidden["clock"].as_f64() <= before["clock"].as_f64()
    {
        return Err("hiding the page interrupted playback".into());
    }
    view.show().map_err(|e| e.to_string())?;
    let active_capture = browser_resource::capture_resource_browser(caller.clone(), id.clone());
    let capturing = tauri::async_runtime::spawn(active_capture);
    tokio::time::sleep(Duration::from_millis(5)).await;
    if raw_close {
        view.close().map_err(|e| e.to_string())?;
    } else {
        browser_resource::close_resource_browser(caller, id).await?;
    }
    let _ = capturing.await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = status(&client, base, &page).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let settled = status(&client, base, &page).await?;
    println!("after close: {after}\nsettled: {settled}");
    if after["at"] != settled["at"] {
        return Err("closed page is still executing".into());
    }
    println!("PASS: both media types started; closed page stopped sending heartbeats");
    Ok(())
}

async fn status(
    client: &reqwest::Client,
    base: &str,
    page: &str,
) -> Result<serde_json::Value, String> {
    client
        .get(format!("{base}/status?id={page}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}
