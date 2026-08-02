//! Safety Demo 的 loopback HTTP、SSE 与静态页宿主。

use std::{convert::Infallible, net::Ipv4Addr, time::Duration};

use async_stream::stream;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{
            CACHE_CONTROL, CONTENT_SECURITY_POLICY, HOST, ORIGIN, REFERRER_POLICY,
            X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
        },
    },
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event, sse::KeepAlive},
    routing::{get, post},
};
use futures_core::Stream;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

use crate::{
    approval::ApprovalError,
    runtime::{DemoRuntime, ResetError, RunControlError, StartRunError},
    wire::{
        ApiErrorBody, ApprovalDecisionRequest, GapNotification, SessionSnapshot, StartRunRequest,
    },
};

const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; \
    base-uri 'none'; frame-ancestors 'none'; form-action 'none'";

#[derive(Clone)]
struct AppState {
    runtime: DemoRuntime,
    expected_authority: String,
    expected_origin: String,
}

/// 已绑定实际 loopback 端口、可以启动伺服的 Demo Server。
pub(crate) struct DemoServer {
    listener: TcpListener,
    router: Router,
    address: std::net::SocketAddr,
}

impl DemoServer {
    pub(crate) async fn bind(port: u16, runtime: DemoRuntime) -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let address = listener.local_addr()?;
        let authority = address.to_string();
        let origin = format!("http://{authority}");
        let state = AppState {
            runtime,
            expected_authority: authority,
            expected_origin: origin,
        };
        Ok(Self {
            listener,
            router: router(state),
            address,
        })
    }

    /// 返回仅监听于本机 loopback 的页面入口。
    pub(crate) fn launch_url(&self) -> String {
        format!("http://{}/", self.address)
    }

    #[cfg(test)]
    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(crate) async fn serve(self) -> std::io::Result<()> {
        axum::serve(self.listener, self.router).await
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/snapshot", get(snapshot))
        .route("/api/events", get(events))
        .route("/api/runs", post(start_run))
        .route("/api/runs/current/cancel", post(cancel_run))
        .route(
            "/api/approvals/{approval_id}/decision",
            post(decide_approval),
        )
        .route("/api/session/reset", post(reset))
        .fallback_service(ServeDir::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/public"
        )))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_boundary,
        ))
        .with_state(state)
}

async fn snapshot(State(state): State<AppState>) -> Json<SessionSnapshot> {
    Json(state.runtime.snapshot().await)
}

async fn reset(State(state): State<AppState>) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .reset()
        .await
        .map(Json)
        .map_err(ApiError::from_reset)
}

async fn start_run(
    State(state): State<AppState>,
    Json(request): Json<StartRunRequest>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .start_run(request)
        .await
        .map(Json)
        .map_err(ApiError::from_start_run)
}

async fn cancel_run(State(state): State<AppState>) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .cancel_run()
        .await
        .map(Json)
        .map_err(ApiError::from_run_control)
}

async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    Json(request): Json<ApprovalDecisionRequest>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .decide_approval(&approval_id, request.decision)
        .await
        .map(Json)
        .map_err(ApiError::from_approval)
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = state.runtime.subscribe();
    let stream = stream! {
        loop {
            match receiver.recv().await {
                Ok(notification) => {
                    if let Ok(json) = serde_json::to_string(&notification) {
                        yield Ok(Event::default().event("notification").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    let gap = GapNotification { skipped };
                    if let Ok(json) = serde_json::to_string(&gap) {
                        yield Ok(Event::default().event("gap").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}

async fn security_boundary(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let validation = validate_request(&state, &request);
    let mut response = match validation {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    };
    apply_security_headers(response.headers_mut());
    response
}

fn validate_request(state: &AppState, request: &Request) -> Result<(), ApiError> {
    let headers = request.headers();
    if single_header_text(headers, HOST).map_err(|_| ApiError::invalid_host())?
        != Some(state.expected_authority.as_str())
    {
        return Err(ApiError::invalid_host());
    }

    let origin = single_header_text(headers, ORIGIN).map_err(|_| ApiError::invalid_origin())?;
    if origin.is_some_and(|origin| origin != state.expected_origin)
        || (*request.method() == Method::POST && origin.is_none())
    {
        return Err(ApiError::invalid_origin());
    }

    Ok(())
}

fn single_header_text(
    headers: &HeaderMap,
    name: axum::http::header::HeaderName,
) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    value.to_str().map(Some).map_err(|_| ())
}

fn apply_security_headers(headers: &mut HeaderMap) {
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY_VALUE),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    fn invalid_host() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_host",
            message: "request Host is not allowed",
        }
    }

    fn invalid_origin() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "invalid_origin",
            message: "request Origin is not allowed",
        }
    }

    fn from_reset(error: ResetError) -> Self {
        match error {
            ResetError::Busy => Self {
                status: StatusCode::CONFLICT,
                code: "session_busy",
                message: "session cannot reset while work is active",
            },
            ResetError::Cleanup(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "workspace_cleanup_failed",
                message: "session reset but previous workspace cleanup failed",
            },
            ResetError::Create(_) | ResetError::Unavailable => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "workspace_reset_failed",
                message: "session workspace reset failed",
            },
        }
    }

    fn from_start_run(error: StartRunError) -> Self {
        match error {
            StartRunError::EmptyMessage => Self {
                status: StatusCode::BAD_REQUEST,
                code: "empty_message",
                message: "message must not be empty",
            },
            StartRunError::Busy => Self {
                status: StatusCode::CONFLICT,
                code: "session_busy",
                message: "another run or pending exchange is active",
            },
            StartRunError::WorkspaceUnavailable | StartRunError::Identifier(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "run_start_failed",
                message: "run could not be started",
            },
        }
    }

    fn from_run_control(_error: RunControlError) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "no_active_run",
            message: "there is no active run to cancel",
        }
    }

    fn from_approval(_error: ApprovalError) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "approval_not_pending",
            message: "approval is no longer pending",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use agent_tools::AbsolutePath;
    use reqwest::Method;
    use serde_json::Value;
    use tokio::task::JoinHandle;

    use super::*;

    struct TestServer {
        base_url: String,
        origin: String,
        runtime: DemoRuntime,
        task: JoinHandle<()>,
        _workdir: tempfile::TempDir,
    }

    impl TestServer {
        async fn start() -> Self {
            let workdir = tempfile::tempdir().expect("create workdir");
            let path = AbsolutePath::new(workdir.path().to_path_buf()).expect("absolute workdir");
            let runtime = DemoRuntime::new(path).await.expect("create runtime");
            let server = DemoServer::bind(0, runtime.clone())
                .await
                .expect("bind test server");
            let base_url = server.base_url();
            assert_eq!(server.launch_url(), format!("{base_url}/"));
            let origin = base_url.clone();
            let task = tokio::spawn(async move {
                server.serve().await.expect("serve test server");
            });
            Self {
                base_url,
                origin,
                runtime,
                task,
                _workdir: workdir,
            }
        }

        async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
            self.runtime.shutdown().await.expect("shutdown runtime");
        }
    }

    #[tokio::test]
    async fn server_static_page_and_api_have_security_headers() {
        let server = TestServer::start().await;
        let response = reqwest::get(&server.base_url)
            .await
            .expect("get static page");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY));
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin")
        );
        assert!(!response.headers().contains_key("set-cookie"));
        let body = response.text().await.expect("read static page");
        assert!(body.contains("Safety Demo"));
        assert!(body.contains("下一次运行"));
        assert!(body.contains("执行检查器"));

        let script = reqwest::get(format!("{}/app.js", server.base_url))
            .await
            .expect("get app script")
            .text()
            .await
            .expect("read app script");
        assert!(script.contains("event_sequence_gap"));
        assert!(script.contains("confirm-build-auto"));
        assert!(script.contains("Allow once"));
        assert!(script.contains("STREAM_RENDER_INTERVAL_MS"));
        assert!(script.contains("renderStreamUpdate"));
        assert!(script.contains("isConversationNearBottom"));
        assert!(!script.contains("innerHTML"));
        assert!(!script.contains("localStorage"));
        assert!(!script.contains("sessionStorage"));

        let style = reqwest::get(format!("{}/app.css", server.base_url))
            .await
            .expect("get app style")
            .text()
            .await
            .expect("read app style");
        assert!(style.contains("height: 100dvh"));
        assert!(style.contains("overscroll-behavior: contain"));

        let snapshot = reqwest::Client::new()
            .get(format!("{}/api/snapshot", server.base_url))
            .send()
            .await
            .expect("get snapshot");
        assert!(!snapshot.headers().contains_key("set-cookie"));
        assert_eq!(snapshot.status(), StatusCode::OK);
        server.stop().await;
    }

    #[tokio::test]
    async fn server_rejects_bad_host_origin_and_options() {
        let server = TestServer::start().await;
        let client = reqwest::Client::new();
        let snapshot_url = format!("{}/api/snapshot", server.base_url);
        let snapshot = client
            .get(&snapshot_url)
            .send()
            .await
            .expect("snapshot request");
        assert_eq!(snapshot.status(), StatusCode::OK);
        assert_eq!(snapshot.headers()[CACHE_CONTROL], "no-store");
        assert!(snapshot.headers().contains_key(CONTENT_SECURITY_POLICY));
        assert_eq!(
            client
                .get(&snapshot_url)
                .header(HOST, "example.invalid")
                .send()
                .await
                .expect("wrong host request")
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            client
                .get(&snapshot_url)
                .header(ORIGIN, "http://example.invalid")
                .send()
                .await
                .expect("wrong origin request")
                .status(),
            StatusCode::FORBIDDEN
        );

        let reset_url = format!("{}/api/session/reset", server.base_url);
        assert_eq!(
            client
                .post(&reset_url)
                .send()
                .await
                .expect("missing origin post")
                .status(),
            StatusCode::FORBIDDEN
        );
        let options = client
            .request(Method::OPTIONS, &reset_url)
            .header(ORIGIN, &server.origin)
            .send()
            .await
            .expect("options request");
        assert_eq!(options.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            !options
                .headers()
                .contains_key("access-control-allow-origin")
        );
        server.stop().await;
    }

    #[test]
    fn duplicate_security_headers_are_not_accepted_as_single_values() {
        let mut headers = HeaderMap::new();
        headers.append(HOST, HeaderValue::from_static("127.0.0.1:1"));
        headers.append(HOST, HeaderValue::from_static("127.0.0.1:2"));
        assert_eq!(single_header_text(&headers, HOST), Err(()));
    }

    #[tokio::test]
    async fn server_snapshot_sse_reset_and_busy_conflict_are_consistent() {
        let server = TestServer::start().await;
        let client = reqwest::Client::new();
        let snapshot_url = format!("{}/api/snapshot", server.base_url);
        let initial: SessionSnapshot = client
            .get(&snapshot_url)
            .send()
            .await
            .expect("get initial snapshot")
            .json()
            .await
            .expect("decode initial snapshot");
        assert_eq!(initial.sequence, 0);

        let mut events = client
            .get(format!("{}/api/events", server.base_url))
            .send()
            .await
            .expect("connect events");
        assert_eq!(events.status(), StatusCode::OK);

        let reset_url = format!("{}/api/session/reset", server.base_url);
        let reset: SessionSnapshot = client
            .post(&reset_url)
            .header(ORIGIN, &server.origin)
            .send()
            .await
            .expect("reset session")
            .json()
            .await
            .expect("decode reset snapshot");
        assert_eq!(reset.sequence, 1);

        let frame = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let bytes = events
                    .chunk()
                    .await
                    .expect("read SSE chunk")
                    .expect("SSE remains open");
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if text.contains("session_reset") {
                    break text;
                }
            }
        })
        .await
        .expect("receive reset notification");
        assert!(frame.contains("\"sequence\":1"));

        let recovered: SessionSnapshot = client
            .get(&snapshot_url)
            .send()
            .await
            .expect("get recovered snapshot")
            .json()
            .await
            .expect("decode recovered snapshot");
        assert_eq!(recovered, reset);

        server.runtime.set_busy_for_test(true, false).await;
        let conflict = client
            .post(&reset_url)
            .header(ORIGIN, &server.origin)
            .send()
            .await
            .expect("busy reset request");
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let error: Value = conflict.json().await.expect("decode conflict error");
        assert_eq!(error["code"], "session_busy");
        server.runtime.set_busy_for_test(false, false).await;
        server.stop().await;
    }

    #[tokio::test]
    async fn server_run_and_approval_control_routes_return_stable_errors() {
        let server = TestServer::start().await;
        let client = reqwest::Client::new();

        let empty_run = client
            .post(format!("{}/api/runs", server.base_url))
            .header(ORIGIN, &server.origin)
            .json(&serde_json::json!({
                "message": "   ",
                "execution_mode": "plan",
                "approval_mode": "ask",
            }))
            .send()
            .await
            .expect("empty run request");
        assert_eq!(empty_run.status(), StatusCode::BAD_REQUEST);
        let error: Value = empty_run.json().await.expect("decode start error");
        assert_eq!(error["code"], "empty_message");

        let cancel = client
            .post(format!("{}/api/runs/current/cancel", server.base_url))
            .header(ORIGIN, &server.origin)
            .send()
            .await
            .expect("cancel request");
        assert_eq!(cancel.status(), StatusCode::CONFLICT);
        let error: Value = cancel.json().await.expect("decode cancel error");
        assert_eq!(error["code"], "no_active_run");

        let approval = client
            .post(format!(
                "{}/api/approvals/approval-missing/decision",
                server.base_url
            ))
            .header(ORIGIN, &server.origin)
            .json(&serde_json::json!({"decision": "allow_once"}))
            .send()
            .await
            .expect("approval request");
        assert_eq!(approval.status(), StatusCode::CONFLICT);
        let error: Value = approval.json().await.expect("decode approval error");
        assert_eq!(error["code"], "approval_not_pending");

        server.stop().await;
    }
}
