//! 默认关闭的私有 Web Demo；静态页面只调用当前 Host 的正式 HTTP API。

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};

use super::{HttpState, auth::validate_demo_host};

const INDEX_HTML: &str = include_str!("web_demo/index.html");
const APP_JS: &str = include_str!("web_demo/app.js");
const CHILD_TASKS_JS: &str = include_str!("web_demo/child-tasks.js");
const STYLES_CSS: &str = include_str!("web_demo/styles.css");

pub(super) fn router(state: HttpState) -> Router<HttpState> {
    Router::new()
        .route("/demo", get(|| async { Redirect::permanent("/demo/") }))
        .route("/demo/", get(index))
        .route("/demo/app.js", get(javascript))
        .route("/demo/child-tasks.js", get(child_tasks_javascript))
        .route("/demo/styles.css", get(styles))
        .layer(middleware::from_fn_with_state(state, validate_request))
}

async fn validate_request(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(error) = validate_demo_host(&state, request.headers()) {
        return error.into_response();
    }
    next.run(request).await
}

async fn index() -> Response {
    static_response("text/html; charset=utf-8", INDEX_HTML, true)
}

async fn javascript() -> Response {
    static_response("text/javascript; charset=utf-8", APP_JS, false)
}

async fn child_tasks_javascript() -> Response {
    static_response("text/javascript; charset=utf-8", CHILD_TASKS_JS, false)
}

async fn styles() -> Response {
    static_response("text/css; charset=utf-8", STYLES_CSS, false)
}

fn static_response(content_type: &'static str, body: &'static str, html: bool) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if html {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; connect-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
            ),
        );
    }
    response
}
