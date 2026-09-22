//! 正式 Host、当前凭据与双协议 Adapter 的确定性 HTTP 闭环。
use super::*;

pub(in crate::access::enterprise::tests) async fn configuration(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
) -> Response {
    mock.model_requests.fetch_add(1, Ordering::SeqCst);
    if !mock.tokens.lock().unwrap().contains_key(&token(&headers)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":"UNAUTHORIZED"}})),
        )
            .into_response();
    }
    let unavailable = mock.model_unavailable.load(Ordering::SeqCst);
    let response = mock.model_configuration.lock().unwrap().clone();
    let held = mock.model_hold.lock().unwrap().take();
    if let Some(held) = held {
        held.started.notify_one();
        held.release.notified().await;
    }
    if unavailable {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(if response.is_null() {
        json!({"state":"unavailable","center_id":CENTER_ID,"reason":"not_selected"})
    } else {
        response
    })
    .into_response()
}

pub(in crate::access::enterprise::tests) async fn proxy(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.proxy_requests
        .lock()
        .unwrap()
        .push((token(&headers), body.clone()));
    let control = mock.proxy_control.lock().unwrap().take();
    let held = mock.proxy_hold.lock().unwrap().take();
    if let Some(held) = held {
        held.started.notify_one();
        held.release.notified().await;
    }
    if let Some((status, code)) = control {
        return (
            StatusCode::from_u16(status).unwrap(),
            [("x-ez-center-control", "1")],
            Json(json!({"error":{"code":code}})),
        )
            .into_response();
    }
    if body.get("messages").is_some() {
        return format!("data: {{\"id\":\"test\",\"model\":{},\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"OK\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"test\",\"model\":{},\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n", body["model"], body["model"]).into_response();
    }
    include_str!(
        "../../../../../../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse"
    )
    .into_response()
}

fn set_configuration(f: &Fixture, model: &str, responses: bool, window: u64) {
    let parameters = ModelParameters {
        context_window_tokens: ModelTokenLimit::Known(window.try_into().unwrap()),
        max_output_tokens: ModelTokenLimit::Known(1024.try_into().unwrap()),
        streaming: ModelFeatureSupport::Supported,
        ..Default::default()
    };
    let origin = f.state.access.center().unwrap().url;
    *f.mock.model_configuration.lock().unwrap() = json!({
        "state":"ready", "center_id": CENTER_ID,
        "endpoint": format!("{origin}/api/llm/providers/01234567-89ab-4cde-8f01-23456789abcd/v1"),
        "provider_type":"openai", "provider_display_name":"测试服务商", "protocol": if responses { "open_ai_responses" } else { "open_ai_chat_completions" },
        "model_id":model, "parameters": parameters,
    });
}

async fn refresh(f: &Fixture, token: &str) -> ModelSettings {
    let response = f
        .command(token, RuntimeCommand::RefreshModelSource {})
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    f.services(token)
        .await
        .runtime
        .get_model_settings()
        .unwrap()
}

async fn validate(f: &Fixture, token: &str) -> ValidateModelConnectionResult {
    let response = f
        .command(
            token,
            RuntimeCommand::ValidateModelConnection(ValidateModelConnectionRequest {
                selection: ModelSelection {
                    provider_instance_id: ProviderInstanceId::new("old-local".to_owned()).unwrap(),
                    model_id: "old-model".to_owned(),
                },
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    serde_json::from_value(value["result"]["payload"]["payload"].clone()).unwrap()
}

#[tokio::test]
async fn managed_models_switch_protocol_and_refresh_without_resending_rejected_calls() {
    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "model-a", false, 8192);
    let alice = f.login("alice").await;
    let settings = refresh(&f, &alice).await;
    assert_eq!(settings.default_model.unwrap().model_id, "model-a");
    assert!(matches!(
        validate(&f, &alice).await.outcome,
        ConnectionValidationOutcome::Succeeded
    ));
    assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), 1);

    set_configuration(&f, "model-b", true, 16384);
    *f.mock.proxy_control.lock().unwrap() = Some((409, "managed_model_configuration_stale".into()));
    assert!(matches!(
        validate(&f, &alice).await.outcome,
        ConnectionValidationOutcome::Failed(_)
    ));
    assert_eq!(
        f.mock.proxy_requests.lock().unwrap().len(),
        2,
        "rejected call must not be resent"
    );
    let runtime = f.services(&alice).await.runtime.clone();
    assert_eq!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .unwrap()
            .model_id,
        "model-b"
    );
    assert!(matches!(
        validate(&f, &alice).await.outcome,
        ConnectionValidationOutcome::Succeeded
    ));
    assert!(
        f.mock
            .proxy_requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .get("input")
            .is_some()
    );

    f.mock.model_unavailable.store(true, Ordering::SeqCst);
    assert!(
        !f.command(&alice, RuntimeCommand::RefreshModelSource {})
            .await
            .status()
            .is_success()
    );
    assert_eq!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .unwrap()
            .model_id,
        "model-b"
    );
    *f.mock.proxy_control.lock().unwrap() = Some((409, "managed_model_unavailable".into()));
    assert!(matches!(
        validate(&f, &alice).await.outcome,
        ConnectionValidationOutcome::Failed(_)
    ));
    assert!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .is_none()
    );
    assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), 4);

    f.mock.model_unavailable.store(false, Ordering::SeqCst);
    *f.mock.model_configuration.lock().unwrap() =
        json!({"state":"unavailable","center_id":CENTER_ID,"reason":"not_selected"});
    assert!(refresh(&f, &alice).await.default_model.is_none());
    f.stop().await;
}

#[tokio::test]
async fn old_login_configuration_cannot_publish_and_new_login_refreshes_existing_domain() {
    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "old-model", false, 8192);
    let held = Arc::new(Held::default());
    *f.mock.model_hold.lock().unwrap() = Some(held.clone());
    let alice = f.login("alice").await;
    let services = f.services(&alice).await;
    held.started.notified().await;
    set_configuration(&f, "new-model", false, 16384);
    let latest = f.login("alice").await;
    let waiting = services.models.as_ref().unwrap().refresh();
    tokio::pin!(waiting);
    // 注册共享等待者后放行旧结果；它必须被丢弃，随后获取新身份对应的配置。
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    held.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        services
            .runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .unwrap()
            .model_id,
        "new-model"
    );
    assert!(matches!(
        validate(&f, &latest).await.outcome,
        ConnectionValidationOutcome::Succeeded
    ));
    assert_eq!(
        f.mock.proxy_requests.lock().unwrap().last().unwrap().0,
        "cl_0000000000000000000000000000000000000000000000000000000000000002"
    );
    f.stop().await;
}

#[tokio::test]
async fn old_key_rejection_does_not_log_out_new_login_and_current_key_rejection_does() {
    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "model-a", false, 8192);
    let alice = f.login("alice").await;
    refresh(&f, &alice).await;
    let held = Arc::new(Held::default());
    *f.mock.proxy_hold.lock().unwrap() = Some(held.clone());
    *f.mock.proxy_control.lock().unwrap() = Some((401, "llm_key_invalid".to_owned()));
    let runtime = f.services(&alice).await.runtime.clone();
    let old = tokio::spawn(async move {
        runtime
            .validate_model_connection(ValidateModelConnectionRequest {
                selection: ModelSelection {
                    provider_instance_id: ProviderInstanceId::new("managed".to_owned()).unwrap(),
                    model_id: "model-a".to_owned(),
                },
            })
            .await
    });
    held.started.notified().await;
    let latest = f.login("alice").await;
    held.release.notify_one();
    old.await.unwrap().unwrap();
    assert_eq!(f.ready(&latest).await.status(), StatusCode::NO_CONTENT);
    assert!(matches!(
        validate(&f, &latest).await.outcome,
        ConnectionValidationOutcome::Succeeded
    ));
    *f.mock.proxy_control.lock().unwrap() = Some((401, "llm_key_invalid".to_owned()));
    // 当前 key 无效会由既有用户域关闭流程中止请求，具体响应可能与取消竞速。
    let _ = f
        .command(
            &latest,
            RuntimeCommand::ValidateModelConnection(ValidateModelConnectionRequest {
                selection: ModelSelection {
                    provider_instance_id: ProviderInstanceId::new("managed".to_owned()).unwrap(),
                    model_id: "model-a".to_owned(),
                },
            }),
        )
        .await;
    assert_eq!(f.ready(&latest).await.status(), StatusCode::UNAUTHORIZED);
    f.stop().await;
}

#[tokio::test]
async fn concurrent_refreshes_share_loading_and_waiter_cancellation_does_not_cancel_it() {
    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "model-a", false, 8192);
    let alice = f.login("alice").await;
    refresh(&f, &alice).await;
    let services = f.services(&alice).await;
    let loader = services.models.as_ref().unwrap().clone();
    set_configuration(&f, "model-a", false, 16384);
    let held = Arc::new(Held::default());
    *f.mock.model_hold.lock().unwrap() = Some(held.clone());
    let before = f.mock.model_requests.load(Ordering::SeqCst);
    let mut cancelled = Box::pin(loader.refresh());
    assert!(futures_util::poll!(cancelled.as_mut()).is_pending());
    held.started.notified().await;
    let mut survivor = Box::pin(loader.refresh());
    assert!(futures_util::poll!(survivor.as_mut()).is_pending());
    drop(cancelled);
    held.release.notify_one();
    survivor.await.unwrap();
    assert_eq!(f.mock.model_requests.load(Ordering::SeqCst), before + 1);
    let selection = services
        .runtime
        .get_model_settings()
        .unwrap()
        .default_model
        .unwrap();
    let parameters = services
        .runtime
        .get_model_configuration(GetModelConfigurationRequest {
            selection,
            origin: ModelConfigOrigin::Online,
        })
        .await
        .unwrap()
        .parameters;
    assert_eq!(
        parameters.context_window_tokens,
        ModelTokenLimit::Known(16384.try_into().unwrap())
    );

    // 关闭用户域必须能中断未返回的 GET 并等待加载任务结束。
    let held = Arc::new(Held::default());
    *f.mock.model_hold.lock().unwrap() = Some(held.clone());
    let pending_loader = loader.clone();
    let pending = tokio::spawn(async move { pending_loader.refresh().await });
    held.started.notified().await;
    f.stop().await;
    assert!(pending.await.unwrap().is_err());
}
