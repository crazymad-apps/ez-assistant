use super::super::{CenterClient, UserState};
use super::*;
use assistant_protocol::{FeedbackTarget, FeedbackTargetKind, HostUserIdentity, SecretValue};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

#[derive(Default)]
struct Mock {
    mode: AtomicUsize,
    posts: AtomicUsize,
    started: Notify,
    release: Notify,
}
async fn submit(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Response {
    mock.posts.fetch_add(1, Ordering::SeqCst);
    let mode = mock.mode.load(Ordering::SeqCst);
    if mode == 4 {
        mock.started.notify_one();
        mock.release.notified().await;
    }
    match mode {
        1 => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"code":"INVALID_REQUEST"}})),
        )
            .into_response(),
        2 => "invalid json".into_response(),
        3 => "x".repeat(65537).into_response(),
        5 => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":"TOKEN_INVALID"}})),
        )
            .into_response(),
        _ => {
            let mut result = body;
            result["id"] = json!("00000000-0000-4000-8000-000000000001");
            result["status"] = json!("pending");
            result["target_kind"] = result["target"]["kind"].clone();
            result["target_name"] = result["target"]["name"].clone();
            result["owner_user_id"] = json!(if mode == 6 { 2 } else { 1 });
            result["owner_username"] = json!("alice");
            result["owner_display_name"] = json!("Alice");
            result["created_at"] = json!("2026-10-02T00:00:00Z");
            result["updated_at"] = result["created_at"].clone();
            for key in [
                "handling_note",
                "handler_user_id",
                "handler_username",
                "handler_display_name",
            ] {
                result[key] = Value::Null;
            }
            Json(result).into_response()
        }
    }
}
async fn fixture() -> (FeedbackAccess, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().route("/api/info", get(|State(mock): State<Arc<Mock>>| async move { Json(json!({"capabilities":if mock.mode.load(Ordering::SeqCst) == 7 { vec![] } else { vec!["feedback"] }})) })).route("/api/feedback", post(submit)).with_state(mock.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let credentials = Arc::new(Credentials::new());
    let identity = HostUserIdentity {
        center_id: "01234567-89ab-4cde-8f01-23456789abcd".into(),
        user_id: 1,
        username: "alice".into(),
        display_name: "Alice".into(),
    };
    let key = UserKey::from(&identity);
    credentials.state.lock().unwrap().users.insert(
        key.clone(),
        UserState {
            login: Arc::new(CenterLogin {
                client: CenterClient::new(url).unwrap(),
                token: SecretValue::new("ct_fixture".into()),
                llm_key: SecretValue::new("cl_fixture".into()),
                checking: Mutex::new(None),
            }),
            identity,
            connections: CancellationToken::new(),
            activity: Arc::new(()),
        },
    );
    (FeedbackAccess::new(credentials, Some(key)), mock, task)
}
fn request() -> SubmitFeedbackRequest {
    SubmitFeedbackRequest {
        submission_id: "00000000-0000-4000-8000-000000000002".into(),
        content: FeedbackContent {
            target: FeedbackTarget {
                kind: FeedbackTargetKind::Platform,
                reference: None,
                name: "platform".into(),
                version: None,
                content_ref: None,
                location: Some("file preview".into()),
            },
            title: " title ".into(),
            description: "description".into(),
            reproduction: None,
            expected_result: None,
        },
        client: FeedbackClientEnvironment {
            client_kind: FeedbackClientKind::Web,
            client_version: None,
            client_os: None,
            client_arch: None,
        },
    }
}
#[tokio::test]
async fn receipt_validation_and_write_uncertainty_never_retry_or_invalidate_normal_errors() {
    let (access, mock, task) = fixture().await;
    let result = access
        .submit(request(), FeedbackSource::Manual)
        .await
        .unwrap();
    assert_eq!(result.summary.title, "title");
    assert_eq!(result.target.name, "EZ Assistant");
    for (mode, expected) in [
        (1, Failure::InvalidInput),
        (2, Failure::SubmissionUnknown),
        (3, Failure::SubmissionUnknown),
        (6, Failure::SubmissionUnknown),
    ] {
        mock.mode.store(mode, Ordering::SeqCst);
        assert_eq!(
            access.submit(request(), FeedbackSource::Manual).await,
            Err(expected)
        );
        assert!(access.login().is_ok());
    }
    assert_eq!(mock.posts.load(Ordering::SeqCst), 5);
    mock.mode.store(7, Ordering::SeqCst);
    assert_eq!(
        access.submit(request(), FeedbackSource::Manual).await,
        Err(Failure::Unsupported)
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 5);
    mock.mode.store(5, Ordering::SeqCst);
    assert_eq!(
        access.submit(request(), FeedbackSource::Manual).await,
        Err(Failure::IdentityChanged)
    );
    assert!(access.login().is_err());
    task.abort();
}
#[tokio::test]
async fn replaced_credentials_and_cancelled_domain_discard_late_receipts() {
    for cancel in [false, true] {
        let (access, mock, task) = fixture().await;
        mock.mode.store(4, Ordering::SeqCst);
        let pending = {
            let access = access.clone();
            tokio::spawn(async move { access.submit(request(), FeedbackSource::Manual).await })
        };
        mock.started.notified().await;
        {
            let mut state = access.credentials.state.lock().unwrap();
            let user = state.users.get_mut(access.key.as_ref().unwrap()).unwrap();
            if cancel {
                user.connections.cancel();
            } else {
                user.login = Arc::new(CenterLogin {
                    client: user.login.client.clone(),
                    token: SecretValue::new("new-login".into()),
                    llm_key: SecretValue::new("new-llm".into()),
                    checking: Mutex::new(None),
                });
            }
        }
        mock.release.notify_one();
        assert_eq!(pending.await.unwrap(), Err(Failure::SubmissionUnknown));
        assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
        task.abort();
    }
}
#[test]
fn content_and_identity_fields_are_bounded_before_io() {
    let mut value = request().content;
    value.target.kind = FeedbackTargetKind::Plugin;
    assert_eq!(normalize_content(value.clone()), Err(Failure::InvalidInput));
    value.target.reference = Some("test-plugin".into());
    assert!(normalize_content(value.clone()).is_ok());
    value.target.reference = Some("../../secret".into());
    assert!(normalize_content(value).is_err());
    let mut value = request().content;
    value.description = "中".repeat(4000);
    value.reproduction = Some("中".repeat(2000));
    assert_eq!(normalize_content(value), Err(Failure::TooLarge));
    let mut raw = serde_json::to_value(request()).unwrap();
    raw["owner_user_id"] = json!(2);
    assert!(serde_json::from_value::<SubmitFeedbackRequest>(raw).is_err());
}
