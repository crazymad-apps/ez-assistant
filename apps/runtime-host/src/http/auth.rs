//! Host、Origin、CORS preflight 与 Bearer Token 校验。

use axum::{
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_REQUEST_HEADERS,
            ACCESS_CONTROL_REQUEST_METHOD, AUTHORIZATION, HOST, ORIGIN, VARY,
        },
    },
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::{HttpState, error::HttpError};

const WEBVIEW_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
    "http://localhost:1420",
];
const ALLOWED_HEADERS: &str = "authorization,content-type";
const ALLOWED_METHODS: &str = "GET,POST,OPTIONS";

pub(super) async fn authorize(
    State(state): State<HttpState>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(error) = validate_host(&state, request.headers()) {
        return error.into_response();
    }
    let origin = match validated_origin(&state, request.headers()) {
        Ok(origin) => origin,
        Err(error) => return error.into_response(),
    };

    if request.method() == Method::OPTIONS {
        return preflight_response(request.headers(), origin.as_deref());
    }
    if !has_valid_bearer(&state, request.headers()) {
        let mut response = HttpError::unauthorized().into_response();
        if let Some(origin) = origin {
            apply_cors(response.headers_mut(), &origin);
        }
        return response;
    }

    let mut response = next.run(request).await;
    if let Some(origin) = origin {
        apply_cors(response.headers_mut(), &origin);
    }
    response
}

fn validate_host(state: &HttpState, headers: &HeaderMap) -> Result<(), HttpError> {
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| HttpError::invalid_request("Host header is required"))?;
    if host == state.authority.as_ref() {
        Ok(())
    } else {
        Err(HttpError::forbidden("Host header is not allowed"))
    }
}

fn validated_origin(state: &HttpState, headers: &HeaderMap) -> Result<Option<String>, HttpError> {
    let Some(origin) = headers.get(ORIGIN) else {
        return Ok(None);
    };
    let origin = origin
        .to_str()
        .map_err(|_| HttpError::forbidden("Origin header is not allowed"))?;
    if origin == state.base_url.as_ref() || WEBVIEW_ORIGINS.contains(&origin) {
        Ok(Some(origin.to_owned()))
    } else {
        Err(HttpError::forbidden("Origin header is not allowed"))
    }
}

fn has_valid_bearer(state: &HttpState, headers: &HeaderMap) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| token == state.access_token.as_ref())
}

fn preflight_response(headers: &HeaderMap, origin: Option<&str>) -> Response {
    let Some(origin) = origin else {
        return HttpError::forbidden("CORS preflight requires an allowed Origin").into_response();
    };
    let requested_method = headers
        .get(ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|value| value.to_str().ok());
    if !matches!(requested_method, Some("GET" | "POST")) {
        return HttpError::forbidden("CORS preflight method is not allowed").into_response();
    }
    if let Some(requested) = headers
        .get(ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|value| value.to_str().ok())
        && requested
            .split(',')
            .map(str::trim)
            .any(|header| !is_allowed_header(header))
    {
        return HttpError::forbidden("CORS preflight headers are not allowed").into_response();
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    apply_cors(response.headers_mut(), origin);
    response
}

fn is_allowed_header(header: &str) -> bool {
    ["authorization", "content-type"]
        .iter()
        .any(|allowed| header.eq_ignore_ascii_case(allowed))
}

fn apply_cors(headers: &mut HeaderMap, origin: &str) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(VARY, HeaderValue::from_static("Origin"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOWED_METHODS),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOWED_HEADERS),
    );
}
