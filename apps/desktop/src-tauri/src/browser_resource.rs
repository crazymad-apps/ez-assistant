//! Desktop 持有的子 WebView；外部网页不获得应用能力。

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Webview, WebviewUrl,
    ipc::Channel,
    webview::{DownloadEvent, NewWindowResponse, PageLoadEvent, WebviewBuilder},
};
use url::Url;

pub struct BrowserResourceManager {
    views: Mutex<HashMap<String, Webview>>,
    next_id: AtomicU64,
    user_agent: Option<String>,
}

impl Default for BrowserResourceManager {
    fn default() -> Self {
        Self {
            views: Mutex::default(),
            next_id: AtomicU64::default(),
            user_agent: desktop_user_agent(),
        }
    }
}

#[cfg(target_os = "macos")]
fn desktop_user_agent() -> Option<String> {
    // WKWebView 默认省略 Safari/Version；WebKit 与系统版本标记使用 Apple 冻结的兼容值。
    // 浏览器版本读取本机 Safari；读取失败时保留系统 UA，不虚构版本。
    for path in [
        "/Applications/Safari.app/Contents/Info.plist",
        "/System/Cryptexes/App/System/Applications/Safari.app/Contents/Info.plist",
    ] {
        let Ok(info) = plist::Value::from_file(path) else {
            continue;
        };
        let Some(version) = info
            .as_dictionary()
            .and_then(|info| info.get("CFBundleShortVersionString"))
            .and_then(plist::Value::as_string)
        else {
            continue;
        };
        if !version.is_empty()
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Some(format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{version} Safari/605.1.15"
            ));
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn desktop_user_agent() -> Option<String> {
    None
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserEvent {
    LoadStarted {
        url: String,
    },
    Loaded {
        url: String,
    },
    Title {
        title: String,
    },
    Popup {
        url: String,
    },
    Notice {
        message: String,
        url: Option<String>,
    },
}

#[derive(Deserialize)]
pub struct BrowserBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
pub struct BrowserViewport {
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    Back,
    Forward,
    Reload,
    Stop,
    Focus,
}

pub fn require_main(caller: &Webview) -> Result<(), String> {
    if caller.label() == "main" {
        Ok(())
    } else {
        Err("browser_forbidden".into())
    }
}

fn http_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "请输入有效的 HTTP 或 HTTPS 地址。")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("只支持不含用户名和密码的 HTTP 或 HTTPS 地址。".into());
    }
    Ok(url)
}

impl BrowserResourceManager {
    fn get(&self, id: &str) -> Result<Webview, String> {
        self.views
            .lock()
            .map_err(|_| "browser_state_unavailable")?
            .get(id)
            .cloned()
            .ok_or_else(|| "browser_not_found".into())
    }

    fn snapshot(&self) -> Result<Vec<Webview>, String> {
        Ok(self
            .views
            .lock()
            .map_err(|_| "browser_state_unavailable")?
            .values()
            .cloned()
            .collect())
    }

    pub fn hide_all(&self) -> Result<(), String> {
        for view in self.snapshot()? {
            view.hide().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn close_all(&self) {
        let views = match self.views.lock() {
            Ok(mut views) => std::mem::take(&mut *views),
            Err(_) => return,
        };
        for view in views.into_values() {
            if let Err(error) = view.close() {
                eprintln!("failed to close browser: {error}");
            }
        }
    }
}

// 原生变更统一串行进入 UI 线程；平台调用前释放状态锁，避免回调重入时死锁。
async fn on_main<T: Send + 'static>(
    app: AppHandle,
    operation: impl FnOnce(AppHandle) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = send.send(operation(handle));
    })
    .map_err(|error| error.to_string())?;
    receive
        .await
        .map_err(|_| "browser_operation_cancelled".to_string())?
}

#[tauri::command]
pub async fn create_resource_browser(
    caller: Webview,
    url: String,
    events: Channel<BrowserEvent>,
) -> Result<String, String> {
    require_main(&caller)?;
    let url = http_url(&url)?;
    on_main(caller.app_handle().clone(), move |app| {
        let manager = app.state::<BrowserResourceManager>();
        let id = format!(
            "resource-browser-{}-{}",
            std::process::id(),
            manager.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let navigation_events = events.clone();
        let title_events = events.clone();
        let popup_events = events.clone();
        let download_events = events.clone();
        let mut builder = WebviewBuilder::new(&id, WebviewUrl::External(url))
            .disable_drag_drop_handler()
            .on_navigation(move |url| {
                let allowed = http_url(url.as_str()).is_ok();
                // 许可回调也包含 iframe 等子框架跳转，不能据此更新整页 URL 或加载状态。
                if !allowed {
                    let _ = navigation_events.send(BrowserEvent::Notice {
                        message: "已阻止非 HTTP/HTTPS 网页跳转。".into(),
                        url: None,
                    });
                }
                allowed
            })
            .on_document_title_changed(move |_, title| {
                let title = title
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(160)
                    .collect();
                let _ = title_events.send(BrowserEvent::Title { title });
            })
            .on_page_load(move |view, payload| {
                let url = view
                    .url()
                    .unwrap_or_else(|_| payload.url().clone())
                    .to_string();
                let event = match payload.event() {
                    PageLoadEvent::Started => BrowserEvent::LoadStarted { url },
                    PageLoadEvent::Finished => BrowserEvent::Loaded { url },
                };
                let _ = events.send(event);
            })
            .on_new_window(move |url, _| {
                let event = if http_url(url.as_str()).is_ok() {
                    BrowserEvent::Popup {
                        url: url.to_string(),
                    }
                } else {
                    BrowserEvent::Notice {
                        message: "已阻止非 HTTP/HTTPS 弹窗。".into(),
                        url: None,
                    }
                };
                let _ = popup_events.send(event);
                NewWindowResponse::Deny
            })
            .on_download(move |_, event| {
                if let DownloadEvent::Requested { url, .. } = event {
                    let _ = download_events.send(BrowserEvent::Notice {
                        message: "请在系统浏览器中下载文件。".into(),
                        url: http_url(url.as_str()).ok().map(|url| url.to_string()),
                    });
                }
                false
            });
        if let Some(user_agent) = &manager.user_agent {
            builder = builder.user_agent(user_agent);
        }
        // 子视图 builder 没有隐藏选项；先在窗口外创建，再隐藏，避免首次出现时闪到正文上。
        let view = caller
            .window()
            .add_child(
                builder,
                LogicalPosition::new(-10000., -10000.),
                LogicalSize::new(1., 1.),
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = view.hide() {
            let _ = view.close();
            return Err(error.to_string());
        }
        match manager.views.lock() {
            Ok(mut views) => {
                views.insert(id.clone(), view);
            }
            Err(_) => {
                let _ = view.close();
                return Err("browser_state_unavailable".into());
            }
        }
        Ok(id)
    })
    .await
}

#[tauri::command]
pub async fn navigate_resource_browser(
    caller: Webview,
    browser_id: String,
    url: String,
) -> Result<(), String> {
    require_main(&caller)?;
    let url = http_url(&url)?;
    on_main(caller.app_handle().clone(), move |app| {
        app.state::<BrowserResourceManager>()
            .get(&browser_id)?
            .navigate(url)
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
pub async fn act_on_resource_browser(
    caller: Webview,
    browser_id: String,
    action: BrowserAction,
) -> Result<(), String> {
    require_main(&caller)?;
    on_main(caller.app_handle().clone(), move |app| {
        let view = app.state::<BrowserResourceManager>().get(&browser_id)?;
        match action {
            BrowserAction::Back => view.eval("history.back()"),
            BrowserAction::Forward => view.eval("history.forward()"),
            BrowserAction::Reload => view.reload(),
            BrowserAction::Stop => view.eval("window.stop()"),
            BrowserAction::Focus => view.set_focus(),
        }
        .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
pub async fn layout_resource_browser(
    caller: Webview,
    browser_id: Option<String>,
    bounds: Option<BrowserBounds>,
    viewport: BrowserViewport,
) -> Result<(), String> {
    require_main(&caller)?;
    on_main(caller.app_handle().clone(), move |app| {
        let manager = app.state::<BrowserResourceManager>();
        for view in manager.snapshot()? {
            if Some(view.label()) != browser_id.as_deref() || bounds.is_none() {
                view.hide().map_err(|error| error.to_string())?;
            }
        }
        if let (Some(id), Some(bounds)) = (browser_id, bounds) {
            let window = caller.window();
            let scale = window.scale_factor().map_err(|error| error.to_string())?;
            let view = manager.get(&id)?;
            let main_size = caller
                .size()
                .map_err(|error| error.to_string())?
                .to_logical::<f64>(scale);
            let main_position = caller
                .position()
                .map_err(|error| error.to_string())?
                .to_logical::<f64>(scale);
            let Some(bounds) = bounds.in_native_viewport(&viewport, main_size, main_position)
            else {
                return view.hide().map_err(|error| error.to_string());
            };
            if !bounds.valid(
                main_position.x + main_size.width,
                main_position.y + main_size.height,
            ) || !window.is_visible().unwrap_or(false)
                || window.is_minimized().unwrap_or(true)
            {
                return view.hide().map_err(|error| error.to_string());
            }
            view.set_bounds(tauri::Rect {
                position: LogicalPosition::new(bounds.x, bounds.y).into(),
                size: LogicalSize::new(bounds.width, bounds.height).into(),
            })
            .map_err(|error| error.to_string())?;
            view.show().map_err(|error| error.to_string())?;
        }
        Ok(())
    })
    .await
}

impl BrowserBounds {
    fn in_native_viewport(
        self,
        viewport: &BrowserViewport,
        size: LogicalSize<f64>,
        position: LogicalPosition<f64>,
    ) -> Option<Self> {
        if !viewport.width.is_finite()
            || !viewport.height.is_finite()
            || viewport.width < 1.
            || viewport.height < 1.
            || !self.valid(viewport.width, viewport.height)
        {
            return None;
        }
        let zoom = size.width / viewport.width;
        // WKWebView 的 DOM 视口扣除了标题栏安全区域，子视图坐标却包含它。
        // 用实际尺寸差推导偏移，避免固定 padding 在不同窗口装饰或缩放下失效。
        let top_inset = (size.height - viewport.height * zoom).max(0.);
        Some(Self {
            x: position.x + self.x * zoom,
            y: position.y + top_inset + self.y * zoom,
            width: self.width * zoom,
            height: self.height * zoom,
        })
    }
    fn valid(&self, width: f64, height: f64) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.x >= 0.
            && self.y >= 0.
            && self.width >= 1.
            && self.height >= 1.
            && self.x + self.width <= width + 1.
            && self.y + self.height <= height + 1.
    }
}

#[tauri::command]
pub async fn resource_browser_url(caller: Webview, browser_id: String) -> Result<String, String> {
    require_main(&caller)?;
    on_main(caller.app_handle().clone(), move |app| {
        app.state::<BrowserResourceManager>()
            .get(&browser_id)?
            .url()
            .map(|url| url.to_string())
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
pub async fn close_resource_browser(caller: Webview, browser_id: String) -> Result<(), String> {
    require_main(&caller)?;
    on_main(caller.app_handle().clone(), move |app| {
        let manager = app.state::<BrowserResourceManager>();
        let view = manager.get(&browser_id)?;
        view.close().map_err(|error| error.to_string())?;
        manager
            .views
            .lock()
            .map_err(|_| "browser_state_unavailable")?
            .remove(&browser_id);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_local_files_scripts_and_credentials() {
        for value in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "tauri://localhost",
            "data:text/html,test",
            "https://user:secret@example.com",
        ] {
            assert!(http_url(value).is_err(), "{value}");
        }
        for value in ["https://example.com/a?q=b", "http://127.0.0.1:3456/"] {
            assert!(http_url(value).is_ok());
        }
    }

    #[test]
    fn native_surface_must_fit_inside_client_area() {
        let mut bounds = BrowserBounds {
            x: 300.,
            y: 100.,
            width: 400.,
            height: 500.,
        };
        assert!(bounds.valid(700., 600.));
        assert!(!bounds.valid(699., 599. - 1.));
        bounds.x = f64::NAN;
        assert!(!bounds.valid(700., 600.));
    }

    #[test]
    fn accounts_for_webview_safe_area_without_fixed_titlebar_padding() {
        let bounds = BrowserBounds {
            x: 421.,
            y: 112.,
            width: 699.,
            height: 616.,
        };
        let native = bounds
            .in_native_viewport(
                &BrowserViewport {
                    width: 1120.,
                    height: 728.,
                },
                LogicalSize::new(1120., 760.),
                LogicalPosition::new(0., 0.),
            )
            .expect("valid viewport");
        assert_eq!(native.y, 144.);
        assert_eq!(native.y + native.height, 760.);
    }
}
