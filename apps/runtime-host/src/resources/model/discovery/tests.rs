//! 公开契约形状的合成夹具与 loopback HTTP；不读取用户配置，也不调用真实 Provider。

use std::{
    collections::VecDeque,
    num::NonZeroU64,
    sync::{Arc, Mutex},
    time::Duration,
};

use assistant_runtime::{
    ModelFeatureSupport as Support, ModelServiceFactory, ModelTokenLimit as Limit,
};
use axum::{Router, body::Body, extract::Request, http::Response};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::resources::model::HostModelServiceFactory;

fn request<'a>(
    provider: &'a agent_types::ProviderId,
    endpoint: &'a str,
) -> ModelDiscoveryRequest<'a> {
    ModelDiscoveryRequest {
        format: match provider.as_str() {
            "dashscope" => DiscoveryFormat::DashScope,
            "vllm" => DiscoveryFormat::Vllm,
            "moonshot" => DiscoveryFormat::Moonshot,
            _ => DiscoveryFormat::OpenAi,
        },
        models_path: if provider.as_str() == "dashscope" {
            "/api/v1/models"
        } else {
            "/v1/models"
        },
        endpoint,
        api_key: "fixture-key-not-a-real-secret",
        connect_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(5),
    }
}

fn known(value: u64) -> Limit {
    Limit::Known(NonZeroU64::new(value).expect("positive fixture"))
}

fn openai(ids: &[&str]) -> Value {
    json!({"object":"list", "data":ids.iter().map(|id| json!({
        "id":id, "object":"model", "owned_by":"fixture"
    })).collect::<Vec<_>>()})
}

fn dashscope(page_no: u64, total: u64, models: Vec<Value>) -> Value {
    json!({"success":true,"output":{
        "page_no":page_no,"page_size":100,"total":total,"models":models
    }})
}

fn parse_fixture(
    document: Value,
    format: DiscoveryFormat,
) -> Result<Vec<DiscoveredModel>, ModelDiscoveryError> {
    parse::page(
        &serde_json::to_vec(&document).expect("fixture JSON"),
        format,
        1,
    )
    .map(|page| page.models)
}

#[test]
fn openai_lists_unknown_models_without_inventing_limits_or_capabilities() {
    let mut document = openai(&["new-model-not-in-catalog"]);
    document["data"][0]["context_window"] = json!(128_000);
    document["data"][0]["max_model_len"] = json!(128_000);
    let models = parse_fixture(document, DiscoveryFormat::OpenAi).expect("valid models");
    assert_eq!(models[0].model_id, "new-model-not-in-catalog");
    assert_eq!(
        models[0].metadata,
        assistant_runtime::ModelParameters::default()
    );
}

#[test]
fn vllm_only_maps_its_declared_context_extension() {
    let mut document = openai(&["served-model"]);
    document["data"][0]["max_model_len"] = json!(32_768);
    let models = parse_fixture(document, DiscoveryFormat::Vllm).expect("valid models");
    assert_eq!(models[0].metadata.context_window_tokens, known(32_768));
    assert_eq!(models[0].metadata.max_output_tokens, Limit::Unknown);
    assert_eq!(models[0].metadata.image_input, Support::Unknown);
}

#[test]
fn dashscope_preserves_mode_specific_limits_and_declared_capabilities() {
    let document = dashscope(
        1,
        1,
        vec![json!({
            "model":"fixture-model", "name":"测试模型", "capabilities":["TG","Reasoning"],
            "features":["function-calling"], "inference_metadata":{"request_modality":["Text","Image"]},
            "model_info":{"context_window":32768,"max_input_tokens":30000,"max_output_tokens":4000,
                "reasoning_max_input_tokens":24000,"reasoning_max_output_tokens":8000}
        })],
    );
    let models = parse_fixture(document, DiscoveryFormat::DashScope).expect("valid metadata");
    let model = &models[0];
    assert_eq!(model.display_name.as_deref(), Some("测试模型"));
    assert_eq!(model.metadata.context_window_tokens, known(32768));
    assert_eq!(model.metadata.max_input_tokens, known(30000));
    assert_eq!(model.metadata.max_output_tokens, known(4000));
    assert_eq!(model.metadata.reasoning_max_input_tokens, known(24000));
    assert_eq!(model.metadata.reasoning_max_output_tokens, known(8000));
    assert_eq!(model.metadata.image_input, Support::Supported);
    assert_eq!(model.metadata.tool_calls, Support::Supported);
    assert_eq!(model.metadata.reasoning, Support::Supported);
}

#[test]
fn null_missing_invalid_and_contradictory_limits_are_not_defaulted() {
    for value in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("8192"),
        json!(9_007_199_254_740_992u64),
        json!({}),
    ] {
        let models = parse_fixture(dashscope(1, 1, vec![json!({
            "model":"fixture", "model_info":{"context_window":100, "max_output_tokens":value}
        })]), DiscoveryFormat::DashScope).expect("invalid field remains visible");
        assert_eq!(models[0].metadata.max_output_tokens, Limit::Invalid);
    }
    let models = parse_fixture(dashscope(1, 2, vec![
        json!({"model":"missing","model_info":{"context_window":null}}),
        json!({"model":"conflicting","model_info":{"context_window":100,"max_output_tokens":101}}),
    ]), DiscoveryFormat::DashScope).expect("metadata can be diagnosed");
    assert_eq!(models[0].metadata.context_window_tokens, Limit::Unknown);
    assert_eq!(models[0].metadata.max_output_tokens, Limit::Unknown);
    assert_eq!(models[1].metadata.max_output_tokens, Limit::Invalid);
}

#[test]
fn absent_capability_is_distinct_from_an_explicit_empty_capability_list() {
    let models = parse_fixture(dashscope(1, 2, vec![
        json!({"model":"unknown"}),
        json!({"model":"unsupported","features":[],"capabilities":[],"inference_metadata":{"request_modality":[]}}),
    ]), DiscoveryFormat::DashScope).expect("valid capability declarations");
    assert_eq!(models[0].metadata.tool_calls, Support::Unknown);
    assert_eq!(models[1].metadata.tool_calls, Support::Unsupported);
    assert_eq!(models[1].metadata.image_input, Support::Unsupported);
    assert_eq!(models[1].metadata.reasoning, Support::Unsupported);
}

#[test]
fn rejects_malformed_envelopes_and_unhandled_pagination() {
    for document in [
        json!({}),
        json!({"object":"list","data":{}}),
        json!({"object":"list","data":[],"has_more":true}),
        openai(&[""]),
        openai(&[" spaced "]),
    ] {
        assert_eq!(
            parse_fixture(document, DiscoveryFormat::OpenAi)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidResponse
        );
    }
    for document in [
        dashscope(2, 1, vec![json!({"model":"wrong-page"})]),
        dashscope(1, 1, vec![]),
        dashscope(
            1,
            1,
            vec![json!({"model":"bad-capability","features":[42]})],
        ),
        dashscope(
            1,
            1,
            vec![json!({"model":"bad-metadata","inference_metadata":42})],
        ),
    ] {
        assert_eq!(
            parse_fixture(document, DiscoveryFormat::DashScope)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidResponse
        );
    }
    assert_eq!(
        parse_fixture(
            json!({"success":false,"message":"fixture-key-not-a-real-secret"}),
            DiscoveryFormat::DashScope
        )
        .unwrap_err()
        .kind(),
        ErrorKind::Unavailable
    );
}

#[test]
fn validates_connection_and_preserves_origin_for_native_dashscope_path() {
    let provider = agent_types::ProviderId::new("dashscope").expect("provider");
    let (url, _) = endpoint(&request(
        &provider,
        "https://workspace.example/compatible-mode/v1/",
    ))
    .expect("same-origin path");
    assert_eq!(url.as_str(), "https://workspace.example/api/v1/models");
    for url in [
        "file:///tmp/models",
        "https://user:key@example/v1",
        "https://example/v1?key=secret",
        "https://example/v1#secret",
    ] {
        assert!(
            matches!(endpoint(&request(&provider, url)), Err(error) if error.kind() == ErrorKind::InvalidConfiguration)
        );
    }
    for path in [
        "https://another.example/models",
        "//another.example/models",
        "/v1/../models",
        "/v1/%2e%2e/models",
        "/models?key=secret",
        "/models#fragment",
        "/v1\\models",
    ] {
        let mut input = request(&provider, "https://example/custom-base");
        input.models_path = path;
        assert!(
            matches!(endpoint(&input), Err(error) if error.kind() == ErrorKind::InvalidConfiguration)
        );
    }
    let mut input = request(&provider, "https://example/custom-base");
    input.models_path = "/another/models";
    assert_eq!(
        endpoint(&input)
            .expect("explicit same-origin override")
            .0
            .as_str(),
        "https://example/another/models"
    );
}

struct Reply {
    status: StatusCode,
    body: Option<Vec<u8>>,
    location: Option<String>,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body: Some(serde_json::to_vec(&value).expect("JSON")),
            location: None,
        }
    }
}

#[derive(Clone, Debug)]
struct SeenRequest {
    path: String,
    authorization: Option<String>,
}

/// 测试拥有监听和 server task；每项结束显式关闭，断言 panic 的 Drop 路径也会 abort。
struct Server {
    endpoint: String,
    requests: Arc<Mutex<Vec<SeenRequest>>>,
    received: Arc<Notify>,
    shutdown: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl Server {
    async fn start(replies: Vec<Reply>) -> Self {
        Self::start_with_transfer(replies, false).await
    }

    async fn start_with_transfer(replies: Vec<Reply>, chunked: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("isolated listener");
        let endpoint = format!(
            "http://{}/v1",
            listener.local_addr().expect("local address")
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::new(Notify::new());
        let pending = Arc::new(Mutex::new(VecDeque::from(replies)));
        let router = Router::new().fallback({
            let requests = requests.clone();
            let received = received.clone();
            move |request: Request| {
                let reply = pending.lock().expect("reply lock").pop_front();
                requests.lock().expect("request lock").push(SeenRequest {
                    path: request.uri().to_string(),
                    authorization: request
                        .headers()
                        .get("authorization")
                        .map(|value| value.to_str().expect("header").to_owned()),
                });
                received.notify_one();
                async move {
                    let reply = reply.expect("unexpected extra request, no implicit retry allowed");
                    let body = match reply.body {
                        Some(bytes) if chunked => {
                            Body::from_stream(futures_util::stream::once(async {
                                Ok::<_, std::io::Error>(bytes)
                            }))
                        }
                        Some(bytes) => Body::from(bytes),
                        None => Body::from_stream(async_stream::stream! {
                            yield Ok::<_, std::io::Error>("{");
                            std::future::pending::<()>().await;
                        }),
                    };
                    let mut response = Response::builder().status(reply.status);
                    if let Some(location) = reply.location {
                        response = response.header("location", location);
                    }
                    response.body(body).expect("response")
                }
            }
        });
        let shutdown = CancellationToken::new();
        let stopped = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .expect("server");
        });
        Self {
            endpoint,
            requests,
            received,
            shutdown,
            task: Some(task),
        }
    }

    async fn close(mut self) {
        self.shutdown.cancel();
        if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
                Ok(result) => result.expect("server task"),
                Err(_) => {
                    task.abort();
                    assert!(task.await.expect_err("aborted server").is_cancelled());
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[tokio::test]
async fn factory_fetches_live_list_each_time_and_does_not_return_previous_success_on_failure() {
    let server = Server::start(vec![
        Reply::json(openai(&["first"])),
        Reply {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: Some(b"fixture secret".to_vec()),
            location: None,
        },
    ])
    .await;
    let home = tempfile::tempdir().expect("isolated home");
    let factory: Box<dyn ModelServiceFactory> = Box::new(HostModelServiceFactory::new(home.path()));
    let provider = agent_types::ProviderId::new("openai").expect("provider");
    let first = factory
        .discover_models(request(&provider, &server.endpoint))
        .await
        .expect("first list");
    assert_eq!(first[0].model_id, "first");
    let failure = factory
        .discover_models(request(&provider, &server.endpoint))
        .await
        .unwrap_err();
    assert_eq!(failure.kind(), ErrorKind::Unavailable);
    assert!(!format!("{failure:?} {failure}").contains("fixture secret"));
    let seen = server.requests.lock().expect("requests").clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].path, "/v1/models");
    assert_eq!(
        seen[0].authorization.as_deref(),
        Some("Bearer fixture-key-not-a-real-secret")
    );
    assert!(
        std::fs::read_dir(home.path())
            .expect("home")
            .next()
            .is_none()
    );
    server.close().await;
}

#[tokio::test]
async fn collects_dashscope_pages_and_rejects_partial_or_inconsistent_results() {
    for (second, expected_error) in [
        (
            Reply::json(dashscope(2, 2, vec![json!({"model":"b"})])),
            None,
        ),
        (
            Reply {
                status: StatusCode::BAD_GATEWAY,
                body: Some(vec![]),
                location: None,
            },
            Some(ErrorKind::Unavailable),
        ),
        (
            Reply::json(dashscope(2, 3, vec![json!({"model":"b"})])),
            Some(ErrorKind::InvalidResponse),
        ),
        (
            Reply::json(dashscope(2, 2, vec![json!({"model":"a"})])),
            Some(ErrorKind::InvalidResponse),
        ),
    ] {
        let server = Server::start(vec![
            Reply::json(dashscope(1, 2, vec![json!({"model":"a"})])),
            second,
        ])
        .await;
        let provider = agent_types::ProviderId::new("dashscope").expect("provider");
        let base = server.endpoint.trim_end_matches("/v1");
        let result = discover(request(&provider, base)).await;
        if let Some(expected) = expected_error {
            assert_eq!(result.unwrap_err().kind(), expected);
        } else {
            let models = result.expect("all pages");
            assert_eq!(
                models
                    .iter()
                    .map(|model| model.model_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["a", "b"]
            );
        }
        let seen = server.requests.lock().expect("requests").clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].path, "/api/v1/models?page_no=2&page_size=100");
        server.close().await;
    }
}

#[tokio::test]
async fn handles_empty_success_without_requiring_a_credential() {
    let server = Server::start(vec![Reply::json(openai(&[]))]).await;
    let provider = agent_types::ProviderId::new("local").expect("provider");
    let mut input = request(&provider, &server.endpoint);
    input.api_key = "";
    assert!(discover(input).await.expect("empty success").is_empty());
    assert!(
        server.requests.lock().expect("requests")[0]
            .authorization
            .is_none()
    );
    server.close().await;
}

#[tokio::test]
async fn rejects_redirects_without_forwarding_credentials_or_retrying() {
    let destination = Server::start(vec![]).await;
    let source = Server::start(vec![Reply {
        status: StatusCode::TEMPORARY_REDIRECT,
        body: Some(vec![]),
        location: Some(destination.endpoint.clone()),
    }])
    .await;
    let provider = agent_types::ProviderId::new("openai").expect("provider");
    assert_eq!(
        discover(request(&provider, &source.endpoint))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::Unavailable
    );
    assert!(destination.requests.lock().expect("requests").is_empty());
    source.close().await;
    destination.close().await;
}

#[tokio::test]
async fn classifies_authentication_rate_limit_and_oversized_body_without_body_leakage() {
    for (status, body, expected) in [
        (
            StatusCode::UNAUTHORIZED,
            b"echo fixture-key-not-a-real-secret".to_vec(),
            ErrorKind::Authentication,
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            vec![],
            ErrorKind::RateLimited,
        ),
        (
            StatusCode::OK,
            vec![b' '; MAX_RESPONSE_BYTES + 1],
            ErrorKind::ResponseTooLarge,
        ),
    ] {
        let server = Server::start(vec![Reply {
            status,
            body: Some(body),
            location: None,
        }])
        .await;
        let provider = agent_types::ProviderId::new("openai").expect("provider");
        let error = discover(request(&provider, &server.endpoint))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), expected);
        assert!(!format!("{error:?} {error}").contains("fixture-key"));
        assert_eq!(server.requests.lock().expect("requests").len(), 1);
        server.close().await;
    }
}

#[tokio::test]
async fn total_timeout_covers_a_body_that_never_finishes() {
    let server = Server::start(vec![Reply {
        status: StatusCode::OK,
        body: None,
        location: None,
    }])
    .await;
    let provider = agent_types::ProviderId::new("openai").expect("provider");
    let mut input = request(&provider, &server.endpoint);
    input.request_timeout = Duration::from_millis(100);
    assert_eq!(
        discover(input).await.unwrap_err().kind(),
        ErrorKind::Timeout
    );
    server.close().await;
}

#[tokio::test]
async fn enforces_chunked_body_and_aggregate_page_budgets() {
    let provider = agent_types::ProviderId::new("dashscope").expect("provider");
    for (replies, expected_requests) in [
        (
            vec![Reply {
                status: StatusCode::OK,
                body: Some(vec![b' '; MAX_RESPONSE_BYTES + 1]),
                location: None,
            }],
            1,
        ),
        (
            (1..=9)
                .map(|page| {
                    let mut reply = Reply::json(dashscope(
                        page,
                        9,
                        vec![json!({"model": format!("model-{page}")})],
                    ));
                    // 每页本身恰好合法；JSON 尾部空白也计入整次发现的下载预算。
                    reply
                        .body
                        .as_mut()
                        .expect("JSON body")
                        .resize(MAX_RESPONSE_BYTES, b' ');
                    reply
                })
                .collect(),
            9,
        ),
    ] {
        let server = Server::start_with_transfer(replies, true).await;
        let result = discover(request(&provider, server.endpoint.trim_end_matches("/v1"))).await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::ResponseTooLarge);
        assert_eq!(
            server.requests.lock().expect("requests").len(),
            expected_requests
        );
        server.close().await;
    }
}

#[tokio::test]
async fn bounds_page_count_and_rejects_excessive_declared_model_count() {
    let provider = agent_types::ProviderId::new("dashscope").expect("provider");
    let server = Server::start(
        (1..=MAX_PAGES)
            .map(|page| {
                Reply::json(dashscope(
                    page,
                    MAX_PAGES + 1,
                    vec![json!({"model": format!("model-{page}")})],
                ))
            })
            .collect(),
    )
    .await;
    assert_eq!(
        discover(request(&provider, server.endpoint.trim_end_matches("/v1")))
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::ResponseTooLarge
    );
    assert_eq!(
        server.requests.lock().expect("requests").len(),
        MAX_PAGES as usize
    );
    server.close().await;
    assert_eq!(
        parse_fixture(
            dashscope(1, MAX_MODELS as u64 + 1, vec![]),
            DiscoveryFormat::DashScope
        )
        .unwrap_err()
        .kind(),
        ErrorKind::ResponseTooLarge
    );
}

#[tokio::test]
async fn dropping_the_caller_future_cancels_without_spawning_more_pages() {
    let server = Server::start(vec![Reply {
        status: StatusCode::OK,
        body: None,
        location: None,
    }])
    .await;
    let provider = agent_types::ProviderId::new("dashscope").expect("provider");
    let base = server.endpoint.trim_end_matches("/v1");
    let mut future = Box::pin(discover(request(&provider, base)));
    tokio::select! {
        _=server.received.notified()=>{},
        result=&mut future=>panic!("unexpected completion: {result:?}"),
        _=tokio::time::sleep(Duration::from_secs(2))=>panic!("request did not arrive"),
    }
    drop(future);
    assert_eq!(server.requests.lock().expect("requests").len(), 1);
    server.close().await;
}

#[test]
fn moonshot_preserves_live_reasoning_options_without_inventing_output_limits() {
    let mut document = openai(&["k3"]);
    document["data"][0] = json!({"id":"k3","object":"model","display_name":"K3",
        "context_length":1048576,"supports_reasoning":true,"supports_image_in":true,
        "supports_dynamic_tools":true,"supports_thinking_type":"only",
        "think_efforts":{"support":true,"valid_efforts":["low","high","max"],"default_effort":"high"}});
    let models = parse_fixture(document.clone(), DiscoveryFormat::Moonshot).expect("live shape");
    assert_eq!(models[0].metadata.context_window_tokens, known(1048576));
    assert_eq!(models[0].metadata.max_output_tokens, Limit::Unknown);
    assert_eq!(
        models[0].metadata.reasoning_mode,
        assistant_runtime::ModelReasoningMode::Always
    );
    assert_eq!(
        models[0].metadata.default_reasoning_effort,
        Some(assistant_runtime::ReasoningEffortKey::High)
    );
    for (field, value) in [
        ("supports_image_in", json!("true")),
        ("supports_reasoning", json!(false)),
        ("supports_thinking_type", json!("unverified")),
        (
            "think_efforts",
            json!({"support":true,"valid_efforts":["low"],"default_effort":"max"}),
        ),
    ] {
        let mut invalid_document = document.clone();
        invalid_document["data"][0][field] = value;
        assert_eq!(
            parse_fixture(invalid_document, DiscoveryFormat::Moonshot)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidResponse
        );
    }
}
