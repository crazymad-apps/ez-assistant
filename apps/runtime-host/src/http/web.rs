//! 正式页面来自编译期文件索引，不把 Runtime Home 或任意文件路径作为网站根目录。

use axum::{
    body::Body,
    http::{
        Request, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

// Safari 的 Blob PDF 会继承页面 CSP；允许同源父页面预览，同时继续禁止外站嵌入。
const WEB_CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' blob: data: http: https:; style-src 'self' 'unsafe-inline'; script-src 'self'; worker-src 'self' blob:; object-src 'none'; frame-src blob:; base-uri 'none'; form-action 'self'; frame-ancestors 'self'";

pub(super) async fn serve(
    axum::extract::State(state): axum::extract::State<super::HttpState>,
    request: Request<Body>,
) -> Response {
    if !matches!(request.method().as_str(), "GET" | "HEAD") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = if request.uri().path() == "/" {
        "/index.html"
    } else {
        request.uri().path()
    };
    let Some((_, mime, bytes)) = WEB_ASSETS.iter().find(|(name, _, _)| *name == path) else {
        if path == "/index.html" && WEB_ASSETS.is_empty() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "此开发 Host 尚未包含 Web 页面，请通过桌面项目的 build:host 构建入口生成。",
            )
                .into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = Response::new(Body::from(*bytes));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, (*mime).parse().expect("static MIME"));
    headers.insert(
        CACHE_CONTROL,
        if path == "/index.html" {
            "no-store"
        } else {
            "public, max-age=31536000, immutable"
        }
        .parse()
        .expect("static cache policy"),
    );
    headers.insert(
        "content-security-policy",
        WEB_CSP
            .replace(
                "connect-src 'self'",
                &format!(
                    "connect-src 'self' {}://{}",
                    if state.secure { "wss" } else { "ws" },
                    request
                        .headers()
                        .get("host")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or(&state.authority)
                ),
            )
            .parse()
            .expect("validated Host CSP"),
    );
    headers.insert(
        "referrer-policy",
        "no-referrer".parse().expect("static policy"),
    );
    headers.insert(
        "x-content-type-options",
        "nosniff".parse().expect("static policy"),
    );
    response
}
