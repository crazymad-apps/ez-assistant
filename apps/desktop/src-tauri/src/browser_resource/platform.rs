//! 网页截图、URL 校验与页面退出的桌面平台边界。macOS 使用 WKWebView 公共接口。
//! objc2/block2 复用 Wry 的固定版本；只在本模块的 FFI 函数允许 unsafe。

/// tauri-runtime-wry 2.11.4 的 WithWebview 将三个 +1 引用 into_raw 后交给回调，
/// PlatformWebview 没有 Drop。集中接管这些引用，避免每次截图/URL 读取泄漏整个网页。
/// 上游修复：https://github.com/tauri-apps/tauri/pull/15224
/// Cargo 精确锁定该版本；升级到上游借用指针实现时必须同时移除此接管逻辑。
#[cfg(target_os = "macos")]
struct NativeView {
    view: objc2::rc::Retained<objc2_web_kit::WKWebView>,
    _controller: objc2::rc::Retained<objc2_web_kit::WKUserContentController>,
    _window: objc2::rc::Retained<objc2_app_kit::NSWindow>,
}

#[cfg(target_os = "macos")]
impl NativeView {
    #[allow(unsafe_code)]
    fn take(platform: tauri::webview::PlatformWebview) -> Option<Self> {
        use objc2::{MainThreadMarker, rc::Retained};
        MainThreadMarker::new()?;
        // SAFETY: 精确核查 tauri-runtime-wry 2.11.4 WithWebview 分支：三个字段各由
        // Retained::into_raw 产生且框架不会 release。此函数消费 PlatformWebview，
        // 每个字段恰好 from_raw 一次，不额外 retain；RAII 在主线程平衡框架转移的 +1。
        unsafe {
            Some(Self {
                view: Retained::from_raw(platform.inner().cast())?,
                _controller: Retained::from_raw(platform.controller().cast())?,
                _window: Retained::from_raw(platform.ns_window().cast())?,
            })
        }
    }
}

/// WKWebView 首次导航尚未提交或失败时 URL 可为 nil。Wry 0.55.1 的 url() 会 unwrap，
/// 因而 URL 轮询与截图校验都走同一空值安全路径；None 表示没有可读取的已提交地址。
#[cfg(target_os = "macos")]
pub(super) async fn current_url(view: &tauri::Webview) -> Result<Option<String>, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    view.with_webview(move |platform| {
        let _ = send.send(read_url(platform));
    })
    .map_err(|_| "browser_not_found".to_string())?;
    receive
        .await
        .map_err(|_| "browser_operation_cancelled".into())
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn current_url(view: &tauri::Webview) -> Result<Option<String>, String> {
    view.url()
        .map(|url| Some(url.to_string()))
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn read_url(platform: tauri::webview::PlatformWebview) -> Option<String> {
    let native = NativeView::take(platform)?;
    // SAFETY: NativeView 持有主线程对象；nil URL 显式返回 None，不跨线程保留对象。
    unsafe { Some(native.view.URL()?.absoluteString()?.to_string()) }
}

#[cfg(target_os = "macos")]
pub(super) async fn preview(
    view: &tauri::Webview,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Option<String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let (send, mut receive) = tokio::sync::mpsc::channel(1);
    view.with_webview(move |platform| start_snapshot(platform, send, permit))
        .ok()?;
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), receive.recv())
        .await
        .ok()??;
    Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes)))
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn preview(
    _view: &tauri::Webview,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn start_snapshot(
    platform: tauri::webview::PlatformWebview,
    send: tokio::sync::mpsc::Sender<Vec<u8>>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    use block2::RcBlock;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError, NSNumber};
    use objc2_web_kit::WKSnapshotConfiguration;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let Some(native) = NativeView::take(platform) else {
        return;
    };
    let webview = &native.view;
    // SAFETY: NativeView 使对象在本次主线程调用期间有效；不从 IPC 接收指针，也不跨线程使用对象。
    // WebKit 复制 completion block 并提供在回调期间有效的 image/error；只借用非空 image。
    // 空的图片编码 properties 满足绑定的泛型约束。图片转为 owned Vec 后才跨线程发送。
    unsafe {
        let size = webview.bounds().size;
        if !size.width.is_finite()
            || !size.height.is_finite()
            || size.width < 1.
            || size.height < 1.
        {
            return;
        }
        let configuration = WKSnapshotConfiguration::new(main_thread);
        // 限制长边为 1024 个逻辑像素；Retina 最多 2048。最终编码再限制为 2 MiB。
        let width = size.width * (1024. / size.width.max(size.height)).min(1.);
        configuration.setSnapshotWidth(Some(&NSNumber::new_f64(width)));
        configuration.setAfterScreenUpdates(false);
        let completion = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
            let _keep_permit = &permit;
            if send.is_closed() {
                return;
            }
            let Some(image) = image.as_ref() else {
                return;
            };
            let Some(tiff) = image.TIFFRepresentation() else {
                return;
            };
            let Some(bitmap) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
            else {
                return;
            };
            let Some(png) = bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            ) else {
                return;
            };
            if png.len() <= 2 * 1024 * 1024 {
                let _ = send.try_send(png.to_vec());
            }
        });
        webview.takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &completion);
    }
}

/// 在 UI 线程先终止页面活动，再让 Tauri 移除子视图；hide 不走这条路径。
/// WKWebView 的原生资源释放可能晚于标签关闭，不能把移出视图树当成停播保证。
pub(super) fn close(view: &tauri::Webview) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let teardown = view
        .with_webview(stop_page)
        .map_err(|error| error.to_string());
    // 即使平台预清理调度失败，也必须尝试释放 Tauri 管理的句柄。
    let closed = view.close().map_err(|error| error.to_string());
    #[cfg(target_os = "macos")]
    teardown?;
    closed
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn stop_page(platform: tauri::webview::PlatformWebview) {
    use objc2::sel;
    use objc2_foundation::{NSObjectProtocol, NSString};

    let Some(native) = NativeView::take(platform) else {
        return;
    };
    let view = &native.view;
    // SAFETY: NativeView 持有存活的主线程 WKWebView。
    // 媒体接口是 macOS 12+ 公共 API，旧系统先检测 selector。只对即将销毁的页面调用，
    // 不在普通隐藏/遮挡时暂停。清除导航代理后载入无脚本空文档，不放宽用户导航协议校验。
    // 不访问私有 _close 或终止共享 WebKit 进程。
    unsafe {
        view.stopLoading();
        if view.respondsToSelector(sel!(setAllMediaPlaybackSuspended:completionHandler:)) {
            // 同时覆盖 HTML 音视频、子框架和 AudioContext，并阻止退出过程中被页面再次启动。
            view.setAllMediaPlaybackSuspended_completionHandler(true, None);
        }
        view.setNavigationDelegate(None);
        view.setUIDelegate(None);
        let _ = view.loadHTMLString_baseURL(&NSString::from_str(""), None);
    }
}
