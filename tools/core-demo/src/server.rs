//! Core Demo 的 loopback HTTP、SSE 与静态页宿主。

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
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

use crate::{
    resources::public_directory,
    runtime::{DemoRuntime, RuntimeError},
    wire::{
        ApiErrorBody, ApprovalDecisionRequest, CreateSessionRequest, GapNotification,
        GlobalSnapshot, MemoryStoreSnapshot, PinMemoryRequest, SessionSnapshot, StartRunRequest,
        UpdateMemoryRequest,
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
        .route("/api/snapshot", get(global_snapshot))
        .route("/api/events", get(events))
        .route("/api/memory", post(pin_memory))
        .route("/api/memory/{memory_id}", post(update_memory))
        .route("/api/memory/{memory_id}/delete", post(unpin_memory))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/{session_id}/snapshot", get(session_snapshot))
        .route("/api/sessions/{session_id}/runs", post(start_run))
        .route(
            "/api/sessions/{session_id}/runs/current/cancel",
            post(cancel_run),
        )
        .route(
            "/api/sessions/{session_id}/approvals/{approval_id}/decision",
            post(decide_approval),
        )
        .fallback_service(ServeDir::new(public_directory()))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_boundary,
        ))
        .with_state(state)
}

async fn global_snapshot(State(state): State<AppState>) -> Json<GlobalSnapshot> {
    Json(state.runtime.snapshot().await)
}

async fn pin_memory(
    State(state): State<AppState>,
    Json(request): Json<PinMemoryRequest>,
) -> Result<Json<MemoryStoreSnapshot>, ApiError> {
    state
        .runtime
        .pin_memory(request)
        .await
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn update_memory(
    State(state): State<AppState>,
    Path(memory_id): Path<String>,
    Json(request): Json<UpdateMemoryRequest>,
) -> Result<Json<MemoryStoreSnapshot>, ApiError> {
    state
        .runtime
        .update_memory(&memory_id, request)
        .await
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn unpin_memory(
    State(state): State<AppState>,
    Path(memory_id): Path<String>,
) -> Result<Json<MemoryStoreSnapshot>, ApiError> {
    state
        .runtime
        .unpin_memory(&memory_id)
        .await
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn session_snapshot(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .session_snapshot(&session_id)
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn create_session(
    State(state): State<AppState>,
    Json(request): Json<CreateSessionRequest>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .create_session(request)
        .await
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn start_run(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<StartRunRequest>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .start_run(&session_id, request)
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn cancel_run(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .cancel_run(&session_id)
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn decide_approval(
    State(state): State<AppState>,
    Path((session_id, approval_id)): Path<(String, String)>,
    Json(request): Json<ApprovalDecisionRequest>,
) -> Result<Json<SessionSnapshot>, ApiError> {
    state
        .runtime
        .decide_approval(&session_id, &approval_id, request.decision)
        .map(Json)
        .map_err(ApiError::from_runtime)
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = event_frames(state.runtime.subscribe())
        .map(|frame| Ok(Event::default().event(frame.event).data(frame.data)));
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}

struct SseFrame {
    event: &'static str,
    data: String,
}

fn event_frames(
    mut receiver: tokio::sync::broadcast::Receiver<crate::wire::EventNotification>,
) -> impl Stream<Item = SseFrame> {
    let stream = stream! {
        loop {
            match receiver.recv().await {
                Ok(notification) => {
                    if let Ok(json) = serde_json::to_string(&notification) {
                        yield SseFrame { event: "notification", data: json };
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    let gap = GapNotification { skipped };
                    if let Ok(json) = serde_json::to_string(&gap) {
                        yield SseFrame { event: "gap", data: json };
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    stream
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

    fn from_runtime(error: RuntimeError) -> Self {
        match error {
            RuntimeError::SessionNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "session_not_found",
                message: "session was not found",
            },
            RuntimeError::EmptyTitle => Self {
                status: StatusCode::BAD_REQUEST,
                code: "empty_title",
                message: "session title must not be empty",
            },
            RuntimeError::TitleTooLong => Self {
                status: StatusCode::BAD_REQUEST,
                code: "title_too_long",
                message: "session title is too long",
            },
            RuntimeError::EmptyMessage => Self {
                status: StatusCode::BAD_REQUEST,
                code: "empty_message",
                message: "message must not be empty",
            },
            RuntimeError::MessageTooLong => Self {
                status: StatusCode::BAD_REQUEST,
                code: "message_too_long",
                message: "message is too long",
            },
            RuntimeError::MemoryInput(_) => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_memory_request",
                message: "the pinned memory request is invalid",
            },
            RuntimeError::SessionBusy => Self {
                status: StatusCode::CONFLICT,
                code: "session_busy",
                message: "session already has an active run",
            },
            RuntimeError::NoActiveRun => Self {
                status: StatusCode::CONFLICT,
                code: "no_active_run",
                message: "there is no active run to cancel",
            },
            RuntimeError::Approval(_) => Self {
                status: StatusCode::CONFLICT,
                code: "approval_not_pending",
                message: "approval is no longer pending",
            },
            RuntimeError::InvalidContextWindow
            | RuntimeError::ModelResources(_)
            | RuntimeError::AgentBuild
            | RuntimeError::Tooling
            | RuntimeError::Memory(_)
            | RuntimeError::Workspace
            | RuntimeError::Path
            | RuntimeError::Identifier => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "internal_error",
                message: "the demo could not complete the request",
            },
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
    use serde_json::Value;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::{cli::ServeArguments, config::ServeConfig, wire::RunStatus};

    struct TestServer {
        base_url: String,
        origin: String,
        runtime: DemoRuntime,
        task: JoinHandle<()>,
        _root: tempfile::TempDir,
    }

    impl TestServer {
        async fn start() -> Self {
            let root = tempfile::tempdir().expect("create temp root");
            let config = ServeConfig::resolve(ServeArguments {
                workdir: root.path().to_path_buf(),
                data_dir: root.path().join("data"),
                port: 0,
                max_compaction_handoffs: crate::cli::DEFAULT_MAX_COMPACTION_HANDOFFS,
                retry_transient: false,
            })
            .expect("resolve config");
            let runtime = DemoRuntime::new_offline(config)
                .await
                .expect("create runtime");
            let server = DemoServer::bind(0, runtime.clone())
                .await
                .expect("bind server");
            let base_url = server.base_url();
            let origin = base_url.clone();
            let task = tokio::spawn(async move {
                server.serve().await.expect("serve test server");
            });
            Self {
                base_url,
                origin,
                runtime,
                task,
                _root: root,
            }
        }

        async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    #[tokio::test]
    async fn static_page_and_api_have_security_headers() {
        let server = TestServer::start().await;
        let response = reqwest::get(&server.base_url).await.expect("get page");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY));
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin")
        );
        let body = response.text().await.expect("read page");
        assert!(body.contains("Core Demo"));

        let script = reqwest::get(format!("{}/app.js", server.base_url))
            .await
            .expect("get script")
            .text()
            .await
            .expect("read script");
        assert!(!script.contains("innerHTML"));
        assert!(!script.contains("localStorage"));
        server.stop().await;
    }

    #[tokio::test]
    async fn rejects_bad_host_and_post_origin() {
        let server = TestServer::start().await;
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{}/api/snapshot", server.base_url))
                .header(HOST, "example.invalid")
                .send()
                .await
                .expect("bad host request")
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            client
                .post(format!("{}/api/sessions", server.base_url))
                .json(&serde_json::json!({}))
                .send()
                .await
                .expect("missing origin request")
                .status(),
            StatusCode::FORBIDDEN
        );
        server.stop().await;
    }

    #[tokio::test]
    async fn session_routes_expose_busy_cancel_and_final_snapshot() {
        let server = TestServer::start().await;
        let client = reqwest::Client::new();
        let session: SessionSnapshot = client
            .post(format!("{}/api/sessions", server.base_url))
            .header(ORIGIN, &server.origin)
            .json(&serde_json::json!({"title": "HTTP session"}))
            .send()
            .await
            .expect("create session")
            .json()
            .await
            .expect("decode session");
        let run_url = format!(
            "{}/api/sessions/{}/runs",
            server.base_url, session.session_id
        );
        let run_request = serde_json::json!({
            "message": "hello",
            "execution_mode": "build",
            "approval_mode": "auto"
        });
        let started: SessionSnapshot = client
            .post(&run_url)
            .header(ORIGIN, &server.origin)
            .json(&run_request)
            .send()
            .await
            .expect("start run")
            .json()
            .await
            .expect("decode run");
        assert!(started.active_run);
        let busy = client
            .post(&run_url)
            .header(ORIGIN, &server.origin)
            .json(&run_request)
            .send()
            .await
            .expect("busy run");
        assert_eq!(busy.status(), StatusCode::CONFLICT);
        let body: Value = busy.json().await.expect("decode busy error");
        assert_eq!(body["code"], "session_busy");

        server.runtime.wait_until_idle(&session.session_id).await;
        let final_snapshot: SessionSnapshot = client
            .get(format!(
                "{}/api/sessions/{}/snapshot",
                server.base_url, session.session_id
            ))
            .send()
            .await
            .expect("get final snapshot")
            .json()
            .await
            .expect("decode final snapshot");
        assert_eq!(
            final_snapshot.run.expect("run").status,
            RunStatus::Completed
        );
        assert_eq!(final_snapshot.journal.len(), 2);
        server.stop().await;
    }

    #[tokio::test]
    async fn memory_routes_persist_latest_store_without_mutating_frozen_session_summary() {
        let server = TestServer::start().await;
        let client = reqwest::Client::new();
        let first: SessionSnapshot = client
            .post(format!("{}/api/sessions", server.base_url))
            .header(ORIGIN, &server.origin)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("create first session")
            .json()
            .await
            .expect("decode first session");
        assert_eq!(first.frozen_prompt.pinned_revision, 0);

        let memory: MemoryStoreSnapshot = client
            .post(format!("{}/api/memory", server.base_url))
            .header(ORIGIN, &server.origin)
            .json(&serde_json::json!({
                "category": "preference",
                "content": "HTTP memory",
                "attributes": {"scope": "demo"}
            }))
            .send()
            .await
            .expect("pin through HTTP")
            .json()
            .await
            .expect("decode memory");
        assert_eq!(memory.revision, 1);
        assert_eq!(memory.entries.len(), 1);

        let unchanged: SessionSnapshot = client
            .get(format!(
                "{}/api/sessions/{}/snapshot",
                server.base_url, first.session_id
            ))
            .send()
            .await
            .expect("get first session")
            .json()
            .await
            .expect("decode first session snapshot");
        assert_eq!(unchanged.frozen_prompt.pinned_revision, 0);

        let second: SessionSnapshot = client
            .post(format!("{}/api/sessions", server.base_url))
            .header(ORIGIN, &server.origin)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("create second session")
            .json()
            .await
            .expect("decode second session");
        assert_eq!(second.frozen_prompt.pinned_revision, 1);
        assert_eq!(second.frozen_prompt.pinned_entry_count, 1);
        server.stop().await;
    }

    #[tokio::test]
    async fn lagged_sse_receiver_emits_gap_and_snapshot_remains_authoritative() {
        let server = TestServer::start().await;
        let receiver = server.runtime.subscribe();
        let frames = event_frames(receiver);
        futures_util::pin_mut!(frames);
        let session = server
            .runtime
            .create_session(CreateSessionRequest::default())
            .await
            .expect("create session");
        server
            .runtime
            .start_run(
                &session.session_id,
                crate::wire::StartRunRequest {
                    message: "generate enough events for a lag gap".to_owned(),
                    execution_mode: crate::wire::ExecutionMode::Plan,
                    approval_mode: crate::wire::ApprovalMode::Ask,
                },
            )
            .expect("start run");
        server.runtime.wait_until_idle(&session.session_id).await;

        let frame = tokio::time::timeout(Duration::from_secs(2), frames.next())
            .await
            .expect("read frame")
            .expect("frame exists");
        assert_eq!(frame.event, "gap");
        let gap: GapNotification = serde_json::from_str(&frame.data).expect("decode gap");
        assert!(gap.skipped > 0);
        let snapshot = server
            .runtime
            .session_snapshot(&session.session_id)
            .expect("session snapshot");
        assert_eq!(snapshot.run.expect("run").status, RunStatus::Completed);
        assert_eq!(snapshot.journal.len(), 2);
        server.stop().await;
    }
}
