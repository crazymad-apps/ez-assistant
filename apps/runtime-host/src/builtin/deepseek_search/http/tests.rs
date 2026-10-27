use super::*;

use axum::{Router, body::Body, extract::State, http::Response, routing::post};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    task::JoinHandle,
};

struct Reply {
    status: u16,
    body: String,
    retry: Option<&'static str>,
    stalled: bool,
    chunked: bool,
}
impl Reply {
    fn ok(value: Value) -> Self {
        Self {
            status: 200,
            body: value.to_string(),
            retry: None,
            stalled: false,
            chunked: false,
        }
    }
    fn status(status: u16) -> Self {
        Self {
            status,
            body: "upstream-private-token".into(),
            retry: Some("0"),
            stalled: false,
            chunked: false,
        }
    }
}
struct StateData {
    replies: Mutex<VecDeque<Reply>>,
    calls: AtomicUsize,
    observed: mpsc::UnboundedSender<Value>,
    release: Arc<Notify>,
}
struct Fixture {
    url: String,
    state: Arc<StateData>,
    observed: mpsc::UnboundedReceiver<Value>,
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}
impl Fixture {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, observed) = mpsc::unbounded_channel();
        let state = Arc::new(StateData {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
            observed: sender,
            release: Arc::new(Notify::new()),
        });
        let router = Router::new()
            .route("/v1/tools/search", post(answer))
            .with_state(state.clone());
        let cancellation = CancellationToken::new();
        let stop = cancellation.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .unwrap();
        });
        Self {
            url,
            state,
            observed,
            cancellation,
            task,
        }
    }
    async fn close(self) {
        self.state.release.notify_waiters();
        self.cancellation.cancel();
        self.task.await.unwrap();
    }
}
async fn answer(
    State(state): State<Arc<StateData>>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response<Body> {
    assert_eq!(headers["authorization"], "Bearer fake-key");
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.observed.send(body).unwrap();
    let reply = state
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("no unexpected retry");
    let body = if reply.stalled {
        let release = state.release.clone();
        Body::from_stream(futures_util::stream::once(async move {
            release.notified().await;
            Ok::<_, std::io::Error>("{}")
        }))
    } else if reply.chunked {
        Body::from_stream(futures_util::stream::once(async move {
            Ok::<_, std::io::Error>(reply.body)
        }))
    } else {
        Body::from(reply.body)
    };
    let mut response = Response::builder().status(reply.status);
    if let Some(retry) = reply.retry {
        response = response.header("retry-after", retry);
    }
    response.body(body).unwrap()
}
fn input() -> SearchInput {
    serde_json::from_value(json!({"query":"example","count":2,"need_content":true})).unwrap()
}
fn result() -> Value {
    json!({"results":[{"rerank_score":1.25,"title":"标题","url":"https://example.org/a?q=1","snippet":"摘要","summary":"总结","content":"网页正文","site_name":null,"site_icon":null,"date":null,"authority":-1,"future_field":{"preserved":true}}],"future_top":"preserved"})
}

#[test]
fn validates_input_and_preserves_query() {
    let input: SearchInput =
        serde_json::from_value(json!({"query":"site:example.com 中文 -排除"})).unwrap();
    assert_eq!(input.validate(), Ok(()));
    assert_eq!(input.payload()["query"], "site:example.com 中文 -排除");
    assert_eq!(input.payload()["count"], 10);
    for value in [
        json!({"query":" "}),
        json!({"query":"x","count":51}),
        json!({"query":"x","ori_question":""}),
        json!({"query":"x","localization":"invalid"}),
        json!({"query":"字".repeat(129)}),
    ] {
        let input: SearchInput = serde_json::from_value(value).unwrap();
        assert_eq!(input.validate(), Err(SearchError::InvalidInput));
    }
}

#[test]
fn caps_retry_after_and_rejects_unsafe_endpoints() {
    assert_eq!(retry_after("999999"), Some(Duration::from_secs(30)));
    assert_eq!(retry_after("invalid"), None);
    let future = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120));
    assert_eq!(retry_after(&future), Some(Duration::from_secs(30)));
    for endpoint in [
        "http://example.com",
        "https://user:secret@example.com",
        "https://example.com?key=secret",
        "file:///tmp/key",
    ] {
        assert!(matches!(
            SearchClient::new(Some("fake"), Some(endpoint)),
            Err(SearchError::InvalidConfiguration)
        ));
    }
    assert!(SearchClient::new(Some("fake"), Some("http://127.0.0.1:1234/v1")).is_ok());
}

#[tokio::test]
async fn preserves_results_and_bounded_retry_succeeds() {
    let mut fixture = Fixture::start(vec![
        Reply::status(503),
        Reply::status(429),
        Reply::ok(result()),
    ])
    .await;
    let client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
    let value = client
        .search(&input(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(value, result());
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        fixture.observed.recv().await.unwrap(),
        json!({"query":"example","count":2,"localization":"","need_content":true})
    );
    fixture.close().await;
}

#[tokio::test]
async fn credentials_failures_do_not_retry_or_expose_upstream() {
    let missing = SearchClient::new(None, Some("http://127.0.0.1:1")).unwrap();
    assert_eq!(
        missing.search(&input(), &CancellationToken::new()).await,
        Err(SearchError::MissingKey)
    );
    for (status, error) in [
        (401, SearchError::Unauthorized),
        (402, SearchError::Balance),
        (400, SearchError::Http),
    ] {
        let fixture = Fixture::start(vec![Reply::status(status)]).await;
        let client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
        assert_eq!(
            client.search(&input(), &CancellationToken::new()).await,
            Err(error)
        );
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        assert!(!error.message().contains("upstream-private-token"));
        fixture.close().await;
    }
}

#[tokio::test]
async fn retry_count_and_response_validation_are_bounded() {
    let fixture = Fixture::start(vec![
        Reply::status(429),
        Reply::status(429),
        Reply::status(429),
    ])
    .await;
    let client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
    assert_eq!(
        client.search(&input(), &CancellationToken::new()).await,
        Err(SearchError::RateLimited)
    );
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 3);
    fixture.close().await;
    for value in [
        json!({}),
        json!({"results":[{"title":"incomplete"}]}),
        json!({"results":null}),
    ] {
        assert_eq!(validate_response(&value), Err(SearchError::InvalidResponse));
    }
    for chunked in [false, true] {
        let mut reply = Reply::ok(result());
        reply.chunked = chunked;
        let fixture = Fixture::start(vec![reply]).await;
        let mut client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
        client.response_limit = 16;
        assert_eq!(
            client.search(&input(), &CancellationToken::new()).await,
            Err(SearchError::ResponseTooLarge)
        );
        assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
        fixture.close().await;
    }
    let mut reply = Reply::ok(json!({}));
    reply.body = "upstream-invalid-json-secret".into();
    let fixture = Fixture::start(vec![reply]).await;
    let client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
    assert_eq!(
        client.search(&input(), &CancellationToken::new()).await,
        Err(SearchError::InvalidResponse)
    );
    fixture.close().await;
}

#[tokio::test]
async fn cancellation_drops_inflight_body_without_retry() {
    let mut reply = Reply::ok(json!({}));
    reply.stalled = true;
    let mut fixture = Fixture::start(vec![reply]).await;
    let client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
    let cancellation = CancellationToken::new();
    let stop = cancellation.clone();
    let task = tokio::spawn(async move { client.search(&input(), &stop).await });
    fixture.observed.recv().await.unwrap();
    cancellation.cancel();
    assert_eq!(task.await.unwrap(), Err(SearchError::Cancelled));
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

#[tokio::test]
async fn response_body_timeout_and_backoff_share_finite_budget() {
    let replies = (0..3)
        .map(|_| {
            let mut reply = Reply::ok(json!({}));
            reply.stalled = true;
            reply
        })
        .collect();
    let fixture = Fixture::start(replies).await;
    let mut client = SearchClient::new(Some("fake-key"), Some(&fixture.url)).unwrap();
    // 缩短同一 attempt 计时器；由服务端保持响应体挂起，不用 sleep 推测网络时序。
    client.attempt_budget = Duration::from_millis(50);
    let started = tokio::time::Instant::now();
    assert_eq!(
        client.search(&input(), &CancellationToken::new()).await,
        Err(SearchError::Timeout)
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(fixture.state.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        ATTEMPT_BUDGET * 3 + Duration::from_secs(30) * 2,
        Duration::from_secs(150)
    );
    fixture.close().await;
}
