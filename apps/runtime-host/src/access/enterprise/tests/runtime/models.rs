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
        empty_configuration()
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
    let request_number = {
        let mut requests = mock.proxy_requests.lock().unwrap();
        requests.push((token(&headers), body.clone()));
        requests.len()
    };
    let scripted = mock.proxy_script.lock().unwrap().pop_front();
    if let Some(response) = scripted {
        return response;
    }
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
        return format!("data: {{\"id\":\"test-{request_number}\",\"model\":{},\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"OK\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"test-{request_number}\",\"model\":{},\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n", body["model"], body["model"]).into_response();
    }
    include_str!(
        "../../../../../../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse"
    )
    .into_response()
}

const MODEL_PROVIDER: &str = "01234567-89ab-4cde-8f01-23456789abcd";

fn empty_configuration() -> Value {
    json!({
        "center_id": CENTER_ID, "organization_id": organization_scope()["organization_id"],
        "allow_member_selection": false, "default_model": null, "providers": [],
    })
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
        "center_id": CENTER_ID, "organization_id": organization_scope()["organization_id"],
        "allow_member_selection": false, "default_model": { "provider_instance_id": MODEL_PROVIDER, "model_id": model },
        "providers": [{
            "provider_instance_id": MODEL_PROVIDER,
            "endpoint": format!("{origin}/api/llm/providers/{MODEL_PROVIDER}/v1"),
            "provider_type":"openai", "provider_display_name":"测试服务商",
            "protocol": if responses { "open_ai_responses" } else { "open_ai_chat_completions" },
            "server_history_enabled":false,"server_history_support":"unknown",
            "models": [{ "state": "ready", "model_id": model, "parameters": parameters, "server_history_binding": null }],
        }],
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
    let selection = f
        .services(token)
        .await
        .runtime
        .get_model_settings()
        .unwrap()
        .default_model
        .unwrap();
    let response = f
        .command(
            token,
            RuntimeCommand::ValidateModelConnection(ValidateModelConnectionRequest { selection }),
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
            .management
            .unwrap()
            .unavailable_reason
            .is_some()
    );
    assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), 4);

    f.mock.model_unavailable.store(false, Ordering::SeqCst);
    *f.mock.model_configuration.lock().unwrap() = empty_configuration();
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
                    source: assistant_protocol::ModelSelectionSource::Organization,
                    provider_instance_id: ProviderInstanceId::new(MODEL_PROVIDER.to_owned())
                        .unwrap(),
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
                    source: assistant_protocol::ModelSelectionSource::Organization,
                    provider_instance_id: ProviderInstanceId::new(MODEL_PROVIDER.to_owned())
                        .unwrap(),
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

#[tokio::test]
async fn managed_proxy_temporary_503_retries_through_real_host_transport() {
    for code in ["llm_proxy_unavailable", "llm_record_unavailable"] {
        let mut f = Fixture::with_domains(true, None).await;
        set_configuration(&f, "model-retry", true, 8192);
        let alice = f.login("alice").await;
        refresh(&f, &alice).await;
        *f.mock.proxy_control.lock().unwrap() = Some((503, code.into()));
        assert!(
            matches!(
                validate(&f, &alice).await.outcome,
                ConnectionValidationOutcome::Succeeded
            ),
            "temporary center error {code} must recover"
        );
        assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), 2);
        f.stop().await;
    }
}

async fn submit_recovery_test(runtime: &AssistantRuntime, session_id: &SessionId) -> RunId {
    let submitted = runtime
        .submit_session_input(assistant_runtime::SubmitSessionInputRequest {
            source: assistant_runtime::InputChannelSource::desktop_text(),
            input: SubmitInputRequest {
                session_id: session_id.clone(),
                message: "continue".into(),
                mode: SubmitInputMode::Normal,
                variant: AgentVariant::Build,
                attachment_ids: vec![],
                quotes: vec![],
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            },
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(run) = runtime
                .list_runs(assistant_protocol::ListRunsRequest {
                    session_id: session_id.clone(),
                })
                .await
                .unwrap()
                .runs
                .into_iter()
                .find(|run| run.input_id == submitted.input_id)
            {
                break run.run_id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn recovery_terminal(
    runtime: &AssistantRuntime,
    session_id: &SessionId,
    run_id: &RunId,
) -> RunSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let run = runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                })
                .await
                .unwrap()
                .run;
            if run.status.is_terminal() {
                return run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovery run terminates")
}

#[tokio::test]
async fn managed_history_recovers_stream_break_expired_id_and_temporary_503_over_http() {
    use futures_util::StreamExt as _;
    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "gpt-test", true, 128_000);
    {
        let mut config = f.mock.model_configuration.lock().unwrap();
        config["providers"][0]["models"][0]["parameters"]["tool_calls"] = json!("supported");
        config["providers"][0]["models"][0]["parameters"]["tool_choice"] = json!({"auto":"supported","none":"supported","required":"supported","named":"supported"});
        config["providers"][0]["server_history_enabled"] = json!(true);
        config["providers"][0]["server_history_support"] = json!("supported");
        config["providers"][0]["models"][0]["server_history_binding"] = json!("a".repeat(64));
    }
    let alice = f.login("alice").await;
    refresh(&f, &alice).await;
    let runtime = f.services(&alice).await.runtime.clone();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    runtime
        .rename_session(RenameSessionRequest {
            session_id: session_id.clone(),
            title: "recovery fixture".into(),
        })
        .await
        .unwrap();
    let first = submit_recovery_test(&runtime, &session_id).await;
    let seed = recovery_terminal(&runtime, &session_id, &first).await;
    assert_eq!(seed.status, RunStatus::Completed, "{:?}", seed.error);

    let break_stream = Arc::new(Notify::new());
    let release = break_stream.clone();
    // Headers and a real SSE event arrive first; only then does the test break the HTTP body.
    let started = futures_util::stream::once(async {
        Ok::<_, std::io::Error>("data: {\"type\":\"response.created\",\"response\":{\"id\":\"broken-response\",\"model\":\"gpt-test\",\"status\":\"in_progress\"}}\n\n".to_owned())
    });
    let interrupted = futures_util::stream::once(async move {
        release.notified().await;
        Err::<String, _>(std::io::Error::other("injected upstream interruption"))
    });
    let broken = Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from_stream(started.chain(interrupted)))
        .unwrap();
    let stale = (StatusCode::BAD_REQUEST, [("x-ez-center-control", "1")], Json(json!({"error":{
        "code":"previous_response_not_found", "type":"invalid_request_error", "param":"previous_response_id", "message":"expired"
    }}))).into_response();
    let unavailable = (
        StatusCode::SERVICE_UNAVAILABLE,
        [("x-ez-center-control", "1")],
        Json(json!({"error":{
            "code":"llm_proxy_unavailable", "message":"temporary upstream connection failure"
        }})),
    )
        .into_response();
    let completed = include_str!(
        "../../../../../../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse"
    )
    .replace("resp_text_1", "resp_text_2")
    .replace("msg_text_1", "msg_text_2")
    .into_response();
    f.mock
        .proxy_script
        .lock()
        .unwrap()
        .extend([broken, stale, unavailable, completed]);
    let mut events = runtime.subscribe_events();
    let run_id = submit_recovery_test(&runtime, &session_id).await;
    let established = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(events.recv().await.unwrap(), RuntimeEvent::ModelStreamEstablished { run_id: ref id, .. } if id == &run_id) {
                break_stream.notify_one();
                break;
            }
        }
    }).await;
    if established.is_err() {
        panic!(
            "not established: {:?}; requests={}",
            runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone()
                })
                .await
                .unwrap()
                .run,
            f.mock.proxy_requests.lock().unwrap().len()
        );
    }
    let run = recovery_terminal(&runtime, &session_id, &run_id).await;
    assert_eq!(run.status, RunStatus::Completed, "{:?}", run.error);
    {
        let requests = f.mock.proxy_requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[1].1.get("previous_response_id").is_some());
        assert_eq!(requests[1].1, requests[2].1);
        assert!(requests[3].1.get("previous_response_id").is_none());
        assert!(
            requests[4].1.get("previous_response_id").is_none(),
            "rebuild retry must not reuse the expired ID"
        );
        assert!(
            requests[3].1 == requests[4].1,
            "rebuild retry must preserve the full request"
        );
    }
    let conversation = runtime.conversation_snapshot(&session_id).await.unwrap();
    assert_eq!(
        conversation
            .messages
            .iter()
            .filter(|m| matches!(m, agent_types::ConversationMessage::User(_)))
            .count(),
        2
    );
    assert!(f.mock.proxy_script.lock().unwrap().is_empty());
    f.stop().await;
}

#[tokio::test]
async fn managed_proxy_invalid_or_oversized_requests_do_not_retry() {
    for (status, code) in [(400, "llm_request_invalid"), (413, "llm_request_too_large")] {
        let mut f = Fixture::with_domains(true, None).await;
        set_configuration(&f, "model-invalid", true, 8192);
        let alice = f.login("alice").await;
        refresh(&f, &alice).await;
        *f.mock.proxy_control.lock().unwrap() = Some((status, code.into()));
        assert!(matches!(
            validate(&f, &alice).await.outcome,
            ConnectionValidationOutcome::Failed(_)
        ));
        assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), 1);
        f.stop().await;
    }
}

#[tokio::test]
async fn sub2api_default_capabilities_execute_through_enterprise_factory() {
    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "gemini-4-argon-high", true, 272000);
    {
        let mut configuration = f.mock.model_configuration.lock().unwrap();
        configuration["providers"][0]["provider_type"] = json!("sub2api");
        configuration["providers"][0]["models"][0]["parameters"]["max_output_tokens"] =
            json!({"state":"unknown"});
        configuration["providers"][0]["models"][0]["parameters"]["streaming"] = json!("unknown");
        configuration["providers"][0]["models"][0]["parameters"]["image_input"] =
            json!("unsupported");
        configuration["providers"][0]["models"][0]["parameters"]["reasoning_efforts"] = json!({});
    }
    let alice = f.login("alice").await;
    let settings = refresh(&f, &alice).await;
    assert!(settings.default_model.is_some());
    let runtime = f.services(&alice).await.runtime.clone();
    let session = runtime
        .create_session(assistant_protocol::CreateSessionRequest::default())
        .await
        .unwrap();
    let run = submit_recovery_test(&runtime, &session.session.session_id).await;
    let status = recovery_terminal(&runtime, &session.session.session_id, &run).await;
    assert_eq!(
        status.status,
        assistant_protocol::RunStatus::Completed,
        "{:?}",
        status.error
    );
    {
        let requests = f.mock.proxy_requests.lock().unwrap();
        assert!(!requests.is_empty());
        assert!(requests[0].1.get("max_output_tokens").is_none());
        assert_eq!(requests[0].1["stream"], json!(true));
        assert!(!requests[0].1["tools"].as_array().unwrap().is_empty());
    }
    f.stop().await;
}

#[tokio::test]
async fn managed_request_retry_and_exhaustion_are_independent_of_center_verification_failure() {
    for exhausted in [false, true] {
        let mut f = Fixture::with_domains(true, None).await;
        // 仅本用例新建的隔离用户配置：缩短退避，仍走正式重试与持久终态路径。
        std::fs::write(f.home.path().join(format!("users/{CENTER_ID}_1/config.toml")),
            "schema_version=1\n[runtime.model_retry]\nretry_on=['unavailable']\ndelays_ms=[1,1]\nmax_retry_after_ms=10\n").unwrap();
        set_configuration(&f, "request-recovery", true, 8192);
        let alice = f.login("alice").await;
        refresh(&f, &alice).await;
        let services = f.services(&alice).await;
        let runtime = &services.runtime;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        runtime
            .rename_session(RenameSessionRequest {
                session_id: session.clone(),
                title: "request recovery".into(),
            })
            .await
            .unwrap();
        let held = Arc::new(Held::default());
        *f.mock.proxy_hold.lock().unwrap() = Some(held.clone());
        *f.mock.proxy_control.lock().unwrap() = Some((503, "llm_proxy_unavailable".into()));
        let run = submit_recovery_test(runtime, &session).await;
        tokio::time::timeout(Duration::from_secs(5), held.started.notified())
            .await
            .unwrap();
        f.mock.unavailable.store(true, Ordering::SeqCst);
        assert_eq!(f.session(&alice).await.status(), StatusCode::OK);
        if exhausted {
            for _ in 0..2 {
                f.mock.proxy_script.lock().unwrap().push_back(
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        [("x-ez-center-control", "1")],
                        Json(json!({"error":{"code":"llm_proxy_unavailable"}})),
                    )
                        .into_response(),
                );
            }
        }
        held.release.notify_one();
        let terminal = recovery_terminal(runtime, &session, &run).await;
        assert_eq!(
            terminal.status,
            if exhausted {
                RunStatus::Failed
            } else {
                RunStatus::Completed
            }
        );
        assert_eq!(
            f.mock.proxy_requests.lock().unwrap().len(),
            if exhausted { 3 } else { 2 }
        );
        assert_eq!(runtime.lifecycle().unwrap(), RuntimeLifecycle::Running);
        let view = runtime
            .get_session_view(GetSessionViewRequest {
                session_id: session,
            })
            .await
            .unwrap();
        assert!(view.snapshot.value.active_run.is_none());
        if exhausted {
            assert_eq!(
                terminal.error.unwrap().code,
                RuntimeErrorCode::ModelExecutionFailed
            );
        }
        f.mock.unavailable.store(false, Ordering::SeqCst);
        assert_eq!(f.ready(&alice).await.status(), StatusCode::NO_CONTENT);
        assert!(Arc::ptr_eq(&services, &f.services(&alice).await));
        f.stop().await;
    }
}

#[tokio::test]
async fn stalled_and_broken_model_streams_emit_retries_while_center_checks_fail() {
    for stalled in [true, false] {
        let mut f = Fixture::with_domains(true, None).await;
        // 新建隔离夹具缩短空闲期限；重试仍使用正式 HTTP Transport 和 Runtime observer。
        std::fs::write(f.home.path().join(format!("users/{CENTER_ID}_1/config.toml")),
            "schema_version=1\n[runtime.model_transport]\nconnect_timeout_ms=50\nrequest_timeout_ms=200\n[runtime.model_retry]\nretry_on=['connection','timeout']\ndelays_ms=[10,10]\nmax_retry_after_ms=10\n").unwrap();
        set_configuration(&f, "stream-retry", true, 8192);
        let alice = f.login("alice").await;
        refresh(&f, &alice).await;
        let services = f.services(&alice).await;
        let runtime = &services.runtime;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        runtime
            .rename_session(RenameSessionRequest {
                session_id: session.clone(),
                title: "stream failure".into(),
            })
            .await
            .unwrap();
        for _ in 0..3 {
            let stream = async_stream::stream! {
                yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b": connected\n\n"));
                if stalled { std::future::pending::<()>().await; }
                else { tokio::time::sleep(Duration::from_millis(50)).await; }
                yield Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "test stream reset"));
            };
            f.mock.proxy_script.lock().unwrap().push_back(
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap(),
            );
        }
        let mut events = runtime.subscribe_event_envelopes();
        f.mock.unavailable.store(true, Ordering::SeqCst);
        assert_eq!(f.session(&alice).await.status(), StatusCode::OK);
        let run = submit_recovery_test(runtime, &session).await;
        let (retries, failure) = tokio::time::timeout(Duration::from_secs(5), async {
            let mut retries = Vec::new();
            loop {
                match events.recv().await.unwrap().event {
                    RuntimeEvent::ModelRetryScheduled {
                        run_id,
                        next_attempt,
                        ..
                    } if run_id == run => retries.push(next_attempt),
                    RuntimeEvent::RunFinished {
                        run_id,
                        status,
                        error,
                        ..
                    } if run_id == run => {
                        assert_eq!(status, RunStatus::Failed);
                        break (retries, error.unwrap());
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(retries, vec![2, 3], "stalled={stalled}, {failure:?}");
        assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), 3);
        assert!(
            failure.message.contains("attempts=3, retries=2"),
            "{failure:?}"
        );
        f.stop().await;
    }
}

#[tokio::test]
async fn organization_and_personal_session_selection_route_and_survive_domain_restart() {
    use assistant_protocol::{
        CreateProviderRequest, ModelSelectionSource, ProviderConnection, ProviderCredentialChange,
        ProviderProtocolPreference, ProviderType, SaveModelFixedConfigRequest, SecretValue,
        SetSessionModelRequest,
    };

    let mut f = Fixture::with_domains(true, None).await;
    set_configuration(&f, "org-default", false, 8192);
    {
        let mut configuration = f.mock.model_configuration.lock().unwrap();
        configuration["allow_member_selection"] = json!(true);
        let mut second = configuration["providers"][0]["models"][0].clone();
        second["model_id"] = json!("shared-model");
        configuration["providers"][0]["models"]
            .as_array_mut()
            .unwrap()
            .push(second);
        let models = configuration["providers"][0]["models"]
            .as_array_mut()
            .unwrap();
        let prototype = models[0].clone();
        for index in 0..110 {
            let mut model = prototype.clone();
            model["model_id"] = json!(format!("fixture-model-{index}"));
            models.push(model);
        }
        assert!(configuration.to_string().len() > 65_536);
    }
    let personal = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", personal.local_addr().unwrap());
    let router = Router::new()
        .route("/v1/chat/completions", post(proxy))
        .with_state(f.mock.clone());
    let upstream = tokio::spawn(async move { axum::serve(personal, router).await.unwrap() });
    let alice = f.login("alice").await;
    let settings = refresh(&f, &alice).await;
    let default = settings.default_model.unwrap();
    let organization = ModelSelection {
        model_id: "shared-model".to_owned(),
        ..default.clone()
    };
    let services = f.services(&alice).await;
    let response = f
        .command(
            &alice,
            RuntimeCommand::CreateProvider(CreateProviderRequest {
                connection: ProviderConnection {
                    display_name: "个人隔离测试".to_owned(),
                    provider_type: ProviderType::Openai,
                    endpoint,
                    protocol_preference: ProviderProtocolPreference::ChatCompletions,
                    models_path: String::new(),
                    discovery_format: ProviderType::Openai.discovery_format(),
                    server_history_enabled: false,
                    server_history_support_override: None,
                },
                credential: ProviderCredentialChange::Replace(SecretValue::new(
                    "fixture-personal-key".to_owned(),
                )),
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let personal = ModelSelection {
        source: ModelSelectionSource::Local,
        provider_instance_id: services
            .runtime
            .list_providers()
            .unwrap()
            .into_iter()
            .find(|provider| provider.source == ModelSelectionSource::Local)
            .unwrap()
            .provider_instance_id,
        model_id: organization.model_id.clone(),
    };
    let params: ModelParameters = serde_json::from_value(
        f.mock.model_configuration.lock().unwrap()["providers"][0]["models"][0]["parameters"]
            .clone(),
    )
    .unwrap();
    let result = f
        .command(
            &alice,
            RuntimeCommand::SaveModelFixedConfig(SaveModelFixedConfigRequest {
                origin: ModelConfigOrigin::Manual,
                selection: personal.clone(),
                parameters: params,
            }),
        )
        .await;
    assert_eq!(result.status(), StatusCode::OK);
    let denied = f
        .command(
            &alice,
            RuntimeCommand::DeleteProvider(assistant_protocol::ProviderRequest {
                source: ModelSelectionSource::Organization,
                provider_instance_id: organization.provider_instance_id.clone(),
            }),
        )
        .await;
    assert!(!denied.status().is_success());
    let session = services
        .runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    services
        .runtime
        .rename_session(RenameSessionRequest {
            session_id: session.clone(),
            title: "双来源".to_owned(),
        })
        .await
        .unwrap();
    let run = submit_recovery_test(&services.runtime, &session).await;
    let terminal = recovery_terminal(&services.runtime, &session, &run).await;
    assert_eq!(
        terminal.status,
        RunStatus::Completed,
        "{:?}",
        terminal.error
    );
    for selection in [organization.clone(), personal.clone()] {
        let result = f
            .command(
                &alice,
                RuntimeCommand::SetSessionModel(SetSessionModelRequest {
                    session_id: session.clone(),
                    model_selection: Some(selection),
                }),
            )
            .await;
        assert_eq!(
            result.status(),
            StatusCode::OK,
            "{}",
            result.text().await.unwrap()
        );
        let run = submit_recovery_test(&services.runtime, &session).await;
        let terminal = recovery_terminal(&services.runtime, &session, &run).await;
        assert_eq!(
            terminal.status,
            RunStatus::Completed,
            "{:?}",
            terminal.error
        );
    }
    {
        let requests = f.mock.proxy_requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].1["model"], "org-default");
        assert_eq!(requests[1].1["model"], "shared-model");
        assert_eq!(requests[0].0, requests[1].0);
        assert!(requests[0].0.starts_with("cl_"));
        assert_eq!(requests[2].1["model"], "shared-model");
        assert_eq!(requests[2].0, "fixture-personal-key");
    }
    *f.mock.model_configuration.lock().unwrap() = empty_configuration();
    assert!(refresh(&f, &alice).await.default_model.is_none());
    let withdrawn = f
        .command(
            &alice,
            RuntimeCommand::SetSessionModel(SetSessionModelRequest {
                session_id: session.clone(),
                model_selection: Some(organization),
            }),
        )
        .await;
    assert!(!withdrawn.status().is_success());
    assert!(
        f.request(reqwest::Method::POST, "/auth/logout", Some(&alice))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    stopped(&services.runtime).await;
    let logged_in = f.login("alice").await;
    let restored = f.services(&logged_in).await;
    let summaries = restored
        .runtime
        .get_application_snapshot(Default::default())
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(
        summaries
            .active_sessions
            .iter()
            .find(|item| item.session_id == session)
            .unwrap()
            .model_selection
            .as_ref(),
        Some(&personal)
    );
    let run = submit_recovery_test(&restored.runtime, &session).await;
    assert_eq!(
        recovery_terminal(&restored.runtime, &session, &run)
            .await
            .status,
        RunStatus::Completed
    );
    assert_eq!(
        f.mock.proxy_requests.lock().unwrap().last().unwrap().0,
        "fixture-personal-key"
    );
    // 已发往个人上游的请求数是撤权前后唯一的副作用证据。
    let before_revoke = f.mock.proxy_requests.lock().unwrap().len();
    f.mock.tokens.lock().unwrap().clear();
    let _ = restored
        .runtime
        .validate_model_connection(ValidateModelConnectionRequest {
            selection: personal,
        })
        .await;
    assert_eq!(f.mock.proxy_requests.lock().unwrap().len(), before_revoke);
    assert_eq!(f.ready(&logged_in).await.status(), StatusCode::UNAUTHORIZED);
    f.stop().await;
    upstream.abort();
    let _ = upstream.await;
}
