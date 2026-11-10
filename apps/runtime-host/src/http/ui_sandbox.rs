//! 无用户服务的固定静态代理；网络响应 CSP 强制 opaque origin，不能改成同源 Blob/srcdoc。

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};

const PROXY_SCRIPT: &str = include_str!("ui_sandbox/proxy.js");
// 响应提供加载上界；每份插件 HTML 的前置 meta 必须再按声明收紧。
// sandbox 不授予 allow-same-origin，因此即使 URL 同源也不能访问主应用 DOM/Cookie。
const CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' https:; style-src 'unsafe-inline' https:; img-src data: blob: https:; media-src data: blob: https:; font-src data: https:; connect-src https: wss:; frame-src blob: data: https:; object-src 'none'; base-uri https:; form-action 'none'; frame-ancestors 'self' tauri://localhost http://tauri.localhost https://tauri.localhost";

pub(super) async fn serve(request: Request<Body>) -> Response {
    if request.uri().query().is_some() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>应用沙箱</title><style>html,body,iframe{{margin:0;width:100%;height:100%;border:0;overflow:hidden}}</style><script>{PROXY_SCRIPT}</script>"
    );
    let mut response = Response::new(Body::from(html));
    let headers = response.headers_mut();
    let policy = if cfg!(debug_assertions) {
        format!("{CSP} http://localhost:1420 http://127.0.0.1:1420")
    } else {
        CSP.to_owned()
    };
    headers.insert(
        "content-type",
        "text/html; charset=utf-8".parse().expect("static MIME"),
    );
    headers.insert(
        "content-security-policy",
        policy.parse().expect("static CSP"),
    );
    headers.insert("cache-control", "no-store".parse().expect("static policy"));
    headers.insert(
        "referrer-policy",
        "no-referrer".parse().expect("static policy"),
    );
    headers.insert(
        "x-content-type-options",
        "nosniff".parse().expect("static policy"),
    );
    headers.insert(
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()"
            .parse()
            .expect("static policy"),
    );
    response
}
