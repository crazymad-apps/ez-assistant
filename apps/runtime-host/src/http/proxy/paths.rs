//! 组网开放范围。两端均检查，内部路由复用 routes 的原业务装配。
use axum::http::{HeaderMap, Uri};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    Command,
    Files,
    Media,
    Events,
}

pub(super) fn route(method: &str, path: &str) -> Option<Route> {
    // ID 不接受转义/路径穿越；本组路由没有业务 query，目标与登录上下文在本机消费。
    let uri: Uri = path.parse().ok()?;
    if uri.scheme().is_some()
        || uri.authority().is_some()
        || uri.query().is_some()
        || path.contains('%')
        || path.contains('\\')
    {
        return None;
    }
    let segments: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return None;
    }
    match (method, segments.as_slice()) {
        ("POST", ["commands"]) => Some(Route::Command),
        ("GET", ["user-terminals", "shells"]) => Some(Route::Files),
        ("GET", ["events"]) => Some(Route::Events),
        ("POST", ["session-materializations"] | ["sessions", _, "attachments"]) => {
            Some(Route::Files)
        }
        (
            "POST",
            [
                "host-files",
                "list" | "select-directory" | "preview" | "download",
            ]
            | [
                "sessions",
                _,
                "resource-files",
                "list" | "stat" | "preview" | "download",
            ],
        ) => Some(Route::Files),
        (
            "GET",
            [
                "sessions",
                _,
                "attachments",
                _,
                "download" | "preview" | "thumbnail",
            ]
            | [
                "sessions",
                _,
                "messages",
                _,
                "resources",
                _,
                "download" | "preview",
            ]
            | [
                "sessions",
                _,
                "child-tasks",
                _,
                "messages",
                _,
                "resources",
                _,
                "download" | "preview",
            ]
            | ["sessions", _, "export.md"],
        ) => Some(Route::Media),
        _ => None,
    }
}
fn allowed_header(name: &str) -> bool {
    matches!(
        name,
        "content-type"
            | "content-length"
            | "accept"
            | "range"
            | "if-range"
            | "if-none-match"
            | "if-modified-since"
    )
}

pub(super) fn headers(headers: &HeaderMap) -> Result<Vec<(String, String)>, ()> {
    if headers.contains_key("content-encoding") {
        return Err(());
    }
    let connection = headers
        .get_all("connection")
        .iter()
        .map(|v| v.to_str().map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    let mut result = Vec::new();
    for name in headers.keys().filter(|name| allowed_header(name.as_str())) {
        // 不能剥掉 multipart/长度等关键头后继续执行一个语义已改变的请求。
        if connection
            .split(',')
            .any(|v| v.trim().eq_ignore_ascii_case(name.as_str()))
        {
            return Err(());
        }
        let values: Vec<_> = headers.get_all(name).iter().collect();
        if values.len() != 1 {
            return Err(());
        }
        let value = values[0].to_str().map_err(|_| ())?;
        if name == "content-length" {
            value.parse::<u64>().map_err(|_| ())?;
        }
        result.push((name.to_string(), value.to_string()));
    }
    Ok(result)
}

pub(super) fn restore(values: &[(String, String)]) -> Result<HeaderMap, ()> {
    let mut result = HeaderMap::new();
    for (name, value) in values {
        if !allowed_header(name) || result.contains_key(name) {
            return Err(());
        }
        result.insert(
            axum::http::HeaderName::try_from(name.as_str()).map_err(|_| ())?,
            axum::http::HeaderValue::try_from(value).map_err(|_| ())?,
        );
    }
    headers(&result)?;
    Ok(result)
}
