//! 易失 Store 与可控发现验证真实 Runtime 接纳边界，无数据库和外网。
use super::*;
use crate::{
    DiscoveredModel, ModelDiscoveryError, ModelDiscoveryErrorKind, ModelDiscoveryFuture,
    ModelDiscoveryRequest,
};
use assistant_protocol::{
    CreateProviderRequest, ModelConfigurationSource, ModelDiscoveryFormat, ModelFeatureSupport,
    ModelParameters, ModelSelection, ModelTokenLimit, ProviderConnection, ProviderCredentialChange,
    ProviderProtocolPreference, ProviderType, SaveModelFixedConfigRequest, UpdateProviderRequest,
};

#[derive(Default)]
struct DiscoveryFactory {
    model: Option<Arc<ScriptedModelService>>,
    fetches: AtomicUsize,
    compiled_limits: Mutex<Vec<(u64, Option<u64>)>>,
    offline: AtomicBool,
    complete_metadata: AtomicBool,
    block: AtomicBool,
    entered: Notify,
    release: Notify,
}
impl ModelServiceFactory for DiscoveryFactory {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        self.compiled_limits
            .lock()
            .unwrap()
            .push((request.context_window_tokens, request.max_input_tokens));
        Ok(ModelServiceBundle::text_only(
            self.model
                .as_ref()
                .map_or_else(empty_model, |model| model.clone() as Arc<dyn ModelService>),
        ))
    }
    fn discover_models<'a>(
        &'a self,
        _request: ModelDiscoveryRequest<'a>,
    ) -> ModelDiscoveryFuture<'a> {
        Box::pin(async move {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            if self.block.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if self.offline.load(Ordering::SeqCst) {
                return Err(ModelDiscoveryError::new(
                    ModelDiscoveryErrorKind::Unavailable,
                ));
            }
            Ok(vec![DiscoveredModel {
                configuration: None,
                model_id: "org/model".into(),
                display_name: None,
                metadata: if self.complete_metadata.load(Ordering::SeqCst) {
                    if self.model.is_some() {
                        model_fixture::parameters()
                    } else {
                        parameters()
                    }
                } else {
                    ModelParameters::default()
                },
            }])
        })
    }
}
fn connection() -> ProviderConnection {
    ProviderConnection {
        display_name: "Test provider".into(),
        provider_type: ProviderType::Openai,
        endpoint: "https://example.test/v1".into(),
        protocol_preference: ProviderProtocolPreference::ChatCompletions,
        models_path: "/v1/models".into(),
        discovery_format: ModelDiscoveryFormat::OpenAi,
    }
}
fn parameters() -> ModelParameters {
    ModelParameters {
        context_window_tokens: ModelTokenLimit::Known(8192.try_into().unwrap()),
        max_output_tokens: ModelTokenLimit::Known(1024.try_into().unwrap()),
        streaming: ModelFeatureSupport::Supported,
        image_input: ModelFeatureSupport::Unsupported,
        tool_calls: ModelFeatureSupport::Unsupported,
        reasoning: ModelFeatureSupport::Unsupported,
        ..Default::default()
    }
}
fn fixture(factory: Arc<DiscoveryFactory>) -> Arc<AssistantRuntime> {
    Arc::new(AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(64).unwrap()),
        Arc::new(MissingConfigSource),
        factory,
        Arc::new(StaticSystemPromptFactory),
        static_run_tool_factory(ToolRegistry::new().snapshot()),
        Arc::new(TestChildWorkspaceFactory::default()),
    ))
}
async fn create(runtime: &AssistantRuntime) -> ModelSelection {
    let provider = runtime
        .create_provider(CreateProviderRequest {
            connection: connection(),
            credential: ProviderCredentialChange::Replace(assistant_protocol::SecretValue::new(
                "private-provider-key".into(),
            )),
        })
        .await
        .unwrap();
    assert!(provider.has_api_key);
    assert!(
        !serde_json::to_string(&provider)
            .unwrap()
            .contains("private-provider-key")
    );
    ModelSelection {
        provider_instance_id: provider.provider_instance_id,
        model_id: "org/model".into(),
    }
}
#[tokio::test]
async fn first_fixed_save_requires_online_identity_but_existing_edit_and_reset_work_offline() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    let forged = ModelSelection {
        model_id: "absent".into(),
        ..selection.clone()
    };
    assert!(
        runtime
            .save_model_fixed_config(SaveModelFixedConfigRequest {
                origin: assistant_protocol::ModelConfigOrigin::Online,
                selection: forged,
                parameters: parameters()
            })
            .await
            .is_err()
    );
    let saved = runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: parameters(),
        })
        .await
        .unwrap();
    factory.offline.store(true, Ordering::SeqCst);
    let before = factory.fetches.load(Ordering::SeqCst);
    let current = runtime
        .get_model_configuration(selection.clone().into())
        .await
        .unwrap();
    assert_eq!(current.source, ModelConfigurationSource::Fixed);
    assert_eq!(current.parameters, saved.parameters);
    let mut next = parameters();
    next.max_output_tokens = ModelTokenLimit::Known(512.try_into().unwrap());
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: next.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        runtime
            .get_model_configuration(selection.clone().into())
            .await
            .unwrap()
            .parameters,
        next
    );
    runtime
        .reset_model_fixed_config(selection.clone())
        .await
        .unwrap();
    runtime
        .reset_model_fixed_config(selection.clone())
        .await
        .unwrap();
    assert_eq!(factory.fetches.load(Ordering::SeqCst), before);
    assert!(
        runtime
            .get_model_configuration(selection.into())
            .await
            .is_err()
    );
    assert_eq!(factory.fetches.load(Ordering::SeqCst), before + 1);
}
#[tokio::test]
async fn provider_edit_is_not_blocked_by_discovery_and_late_result_is_rejected() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    factory.block.store(true, Ordering::SeqCst);
    let request_runtime = runtime.clone();
    let id = selection.provider_instance_id.clone();
    let pending = tokio::spawn(async move { request_runtime.list_provider_models(id).await });
    factory.entered.notified().await;
    tokio::time::timeout(
        Duration::from_secs(1),
        runtime.update_provider(UpdateProviderRequest {
            provider_instance_id: selection.provider_instance_id,
            connection: connection(),
            credential: ProviderCredentialChange::Unchanged,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    factory.release.notify_one();
    assert!(matches!(
        pending.await.unwrap(),
        Err(RuntimeError::ConfigurationConflict)
    ));
}
#[tokio::test]
async fn deletion_during_first_fixed_save_cannot_resurrect_provider_or_fixed_record() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    factory.block.store(true, Ordering::SeqCst);
    let request_runtime = runtime.clone();
    let target = selection.clone();
    let pending = tokio::spawn(async move {
        request_runtime
            .save_model_fixed_config(SaveModelFixedConfigRequest {
                origin: assistant_protocol::ModelConfigOrigin::Online,
                selection: target,
                parameters: parameters(),
            })
            .await
    });
    factory.entered.notified().await;
    runtime
        .delete_provider(selection.provider_instance_id.clone())
        .await
        .unwrap();
    factory.release.notify_one();
    assert!(matches!(
        pending.await.unwrap(),
        Err(RuntimeError::ConfigurationConflict)
    ));
    assert!(runtime.list_providers().unwrap().is_empty());
    assert!(
        runtime
            .store
            .get_model_fixed_config(selection)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn invalid_connection_and_parameters_are_rejected_before_network_or_storage() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let mut invalid = connection();
    invalid.models_path = "https://other-host.test/models".into();
    assert!(
        runtime
            .create_provider(CreateProviderRequest {
                connection: invalid,
                credential: ProviderCredentialChange::Clear
            })
            .await
            .is_err()
    );
    assert!(runtime.list_providers().unwrap().is_empty());
    let selection = create(&runtime).await;
    assert!(
        runtime
            .save_model_fixed_config(SaveModelFixedConfigRequest {
                origin: assistant_protocol::ModelConfigOrigin::Online,
                selection,
                parameters: ModelParameters::default()
            })
            .await
            .is_err()
    );
    assert_eq!(factory.fetches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fixed_save_invalidates_an_online_read_that_started_without_a_fixed_record() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    factory.block.store(true, Ordering::SeqCst);
    let reader_runtime = runtime.clone();
    let reader_selection = selection.clone();
    let pending = tokio::spawn(async move {
        reader_runtime
            .get_model_configuration(reader_selection.into())
            .await
    });
    factory.entered.notified().await;
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: parameters(),
        })
        .await
        .unwrap();
    factory.release.notify_one();
    assert!(matches!(
        pending.await.unwrap(),
        Err(RuntimeError::ConfigurationConflict)
    ));
    let detail = runtime
        .get_model_configuration(selection.into())
        .await
        .unwrap();
    assert_eq!(detail.source, ModelConfigurationSource::Fixed);
    assert_eq!(detail.parameters, parameters());
}

#[tokio::test]
async fn provider_change_rejects_fixed_capabilities_that_the_target_protocol_cannot_encode() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    let mut responses = connection();
    responses.protocol_preference = ProviderProtocolPreference::Responses;
    runtime
        .update_provider(UpdateProviderRequest {
            provider_instance_id: selection.provider_instance_id.clone(),
            connection: responses.clone(),
            credential: ProviderCredentialChange::Unchanged,
        })
        .await
        .unwrap();
    let mut fixed = parameters();
    fixed.image_input = ModelFeatureSupport::Supported;
    fixed.tool_calls = ModelFeatureSupport::Supported;
    fixed.tool_image_projection = assistant_protocol::ModelToolImageProjection::NativeToolResult;
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: fixed.clone(),
        })
        .await
        .unwrap();
    let before = factory.fetches.load(Ordering::SeqCst);
    let mut vllm = connection();
    vllm.provider_type = ProviderType::Vllm;
    vllm.protocol_preference = ProviderProtocolPreference::Auto;
    assert!(
        runtime
            .update_provider(UpdateProviderRequest {
                provider_instance_id: selection.provider_instance_id.clone(),
                connection: vllm,
                credential: ProviderCredentialChange::Clear,
            })
            .await
            .is_err()
    );
    assert_eq!(runtime.list_providers().unwrap()[0].connection, responses);
    assert!(runtime.list_providers().unwrap()[0].has_api_key);
    assert_eq!(
        runtime
            .get_model_configuration(selection.into())
            .await
            .unwrap()
            .parameters,
        fixed
    );
    assert_eq!(factory.fetches.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn selections_use_database_references_and_clear_offline_without_changing_other_purposes() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    let missing = runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest {
            selection: Some(selection.clone()),
        })
        .await;
    assert!(missing.is_err(), "online identity alone is not sufficient");
    assert!(
        runtime
            .get_model_settings()
            .unwrap()
            .default_model
            .is_none()
    );
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: parameters(),
        })
        .await
        .unwrap();
    let selected = runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest {
            selection: Some(selection.clone()),
        })
        .await
        .unwrap();
    assert_eq!(selected.default_model, Some(selection.clone()));
    assert!(
        runtime
            .set_auxiliary_vision_model(assistant_protocol::SetAuxiliaryVisionModelRequest {
                selection: Some(selection.clone()),
            })
            .await
            .is_err(),
        "unknown image capability cannot be chosen for vision"
    );
    let mut vision = parameters();
    vision.image_input = ModelFeatureSupport::Supported;
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection: selection.clone(),
            parameters: vision,
        })
        .await
        .unwrap();
    runtime
        .set_auxiliary_vision_model(assistant_protocol::SetAuxiliaryVisionModelRequest {
            selection: Some(selection.clone()),
        })
        .await
        .unwrap();
    factory.offline.store(true, Ordering::SeqCst);
    assert!(
        runtime
            .set_default_model(assistant_protocol::SetDefaultModelRequest {
                selection: Some(selection.clone()),
            })
            .await
            .is_ok(),
        "saved fixed models remain selectable without online identity"
    );
    let before = factory.fetches.load(Ordering::SeqCst);
    let cleared = runtime
        .set_auxiliary_vision_model(assistant_protocol::SetAuxiliaryVisionModelRequest {
            selection: None,
        })
        .await
        .unwrap();
    assert_eq!(cleared.default_model, Some(selection.clone()));
    assert!(cleared.vision_model.is_none());
    assert_eq!(runtime.store.load_model_settings().await.unwrap(), cleared);
    runtime
        .delete_provider(selection.provider_instance_id)
        .await
        .unwrap();
    runtime.restore_model_settings().await.unwrap();
    assert_eq!(
        runtime.get_model_settings().unwrap(),
        cleared,
        "deleted provider references stay visible"
    );
    runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest { selection: None })
        .await
        .unwrap();
    assert_eq!(factory.fetches.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn late_selection_after_provider_change_never_commits_stale_reference() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    factory.complete_metadata.store(true, Ordering::SeqCst);
    factory.block.store(true, Ordering::SeqCst);
    let pending_runtime = runtime.clone();
    let selected = selection.clone();
    let pending = tokio::spawn(async move {
        pending_runtime
            .set_default_model(assistant_protocol::SetDefaultModelRequest {
                selection: Some(selected),
            })
            .await
    });
    factory.entered.notified().await;
    runtime
        .update_provider(UpdateProviderRequest {
            provider_instance_id: selection.provider_instance_id,
            connection: connection(),
            credential: ProviderCredentialChange::Unchanged,
        })
        .await
        .unwrap();
    factory.release.notify_one();
    assert!(matches!(
        pending.await.unwrap(),
        Err(RuntimeError::ConfigurationConflict)
    ));
    assert!(
        runtime
            .store
            .load_model_settings()
            .await
            .unwrap()
            .default_model
            .is_none()
    );
}

async fn online_execution_fixture() -> (
    Arc<AssistantRuntime>,
    Arc<DiscoveryFactory>,
    ModelSelection,
    assistant_protocol::SessionId,
) {
    let factory = Arc::new(DiscoveryFactory::default());
    factory.complete_metadata.store(true, Ordering::SeqCst);
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest {
            selection: Some(selection.clone()),
        })
        .await
        .unwrap();
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session;
    assert!(
        session.model_selection.is_none(),
        "following default remains a nullable reference"
    );
    (runtime, factory, selection, session.session_id)
}

fn input_request(session_id: &assistant_protocol::SessionId, message: &str) -> SubmitInputRequest {
    SubmitInputRequest {
        mode: assistant_protocol::SubmitInputMode::Normal,
        variant: assistant_protocol::AgentVariant::Build,
        session_id: session_id.clone(),
        message: message.into(),
        attachment_ids: vec![],
        quotes: vec![],
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    }
}

#[tokio::test]
async fn slow_discovery_allows_queueing_and_cancellation_without_starting_cancelled_input() {
    let (runtime, factory, _, session_id) = online_execution_fixture().await;
    factory.block.store(true, Ordering::SeqCst);
    let first = runtime
        .submit_input(input_request(&session_id, "cancel this"))
        .await
        .unwrap();
    factory.entered.notified().await;
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        runtime.submit_input(input_request(&session_id, "keep this")),
    )
    .await
    .expect("discovery must not block enqueue")
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.cancel_queued_input(assistant_protocol::CancelQueuedInputRequest {
            session_id: session_id.clone(),
            input_id: first.input_id.clone(),
        }),
    )
    .await
    .expect("discovery must not block cancellation")
    .unwrap();
    // 被取消 Input 的目录仍未释放；后继 Input 必须能独立准备和执行。
    wait_for_terminal(&runtime, &session_id, &second.run.run_id).await;
    let state = runtime.session_for_test(&session_id).await;
    assert!(
        !state
            .lock_state()
            .unwrap()
            .runs
            .contains_key(&first.run.run_id)
    );
    let conversation = runtime.conversation_snapshot(&session_id).await.unwrap();
    let body = serde_json::to_string(&conversation).unwrap();
    assert!(!body.contains("cancel this"));
    assert!(body.contains("keep this"));
}

#[tokio::test]
async fn changing_default_during_preparation_rejects_old_execution_before_message_commit() {
    let (runtime, factory, _, session_id) = online_execution_fixture().await;
    factory.block.store(true, Ordering::SeqCst);
    let input = runtime
        .submit_input(input_request(&session_id, "uncommitted"))
        .await
        .unwrap();
    factory.entered.notified().await;
    runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest { selection: None })
        .await
        .unwrap();
    factory.release.notify_one();
    let result = wait_for_terminal(&runtime, &session_id, &input.run.run_id).await;
    assert_eq!(result.status, assistant_protocol::RunStatus::Failed);
    assert_eq!(
        result.error.unwrap().code,
        assistant_protocol::RuntimeErrorCode::ConfigurationConflict
    );
    assert!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}

#[tokio::test]
async fn runtime_shutdown_cancels_discovery_that_has_not_returned() {
    let (runtime, factory, _, session_id) = online_execution_fixture().await;
    factory.block.store(true, Ordering::SeqCst);
    runtime
        .submit_input(input_request(&session_id, "pending"))
        .await
        .unwrap();
    factory.entered.notified().await;
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.shutdown(ShutdownRuntimeRequest::default()),
    )
    .await
    .expect("shutdown must cancel discovery")
    .unwrap();
    assert!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}

#[tokio::test]
async fn clearing_effort_without_a_model_does_not_discover_or_block_recovery() {
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session;
    runtime
        .set_session_reasoning_effort(assistant_protocol::SetSessionReasoningEffortRequest {
            session_id: session.session_id,
            effort: None,
        })
        .await
        .unwrap();
    assert_eq!(factory.fetches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fixed_execution_does_not_fetch_an_offline_catalog() {
    let (runtime, factory, selection, session_id) = online_execution_fixture().await;
    let mut fixed = parameters();
    fixed.max_input_tokens = ModelTokenLimit::Known(2048.try_into().unwrap());
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            origin: assistant_protocol::ModelConfigOrigin::Online,
            selection,
            parameters: fixed,
        })
        .await
        .unwrap();
    factory.offline.store(true, Ordering::SeqCst);
    let before = factory.fetches.load(Ordering::SeqCst);
    let submitted = runtime
        .submit_input(input_request(&session_id, "fixed offline execution"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &submitted.run.run_id).await;
    let body =
        serde_json::to_string(&runtime.conversation_snapshot(&session_id).await.unwrap()).unwrap();
    assert!(body.contains("fixed offline execution"));
    assert_eq!(factory.fetches.load(Ordering::SeqCst), before);
    assert!(
        factory
            .compiled_limits
            .lock()
            .unwrap()
            .contains(&(8192, Some(2048)))
    );
}

#[tokio::test]
async fn title_compaction_and_goal_reuse_session_parameters_without_discovery() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&assistant_text("answer", "answer"))),
            ModelScript::Events(message_events(&assistant_title_tool_call(
                "title",
                "title-call",
                "复用模型",
            ))),
        ],
    ));
    let factory = Arc::new(DiscoveryFactory {
        model: Some(model.clone()),
        ..Default::default()
    });
    factory.complete_metadata.store(true, Ordering::SeqCst);
    let runtime = fixture(factory.clone());
    let selection = create(&runtime).await;
    runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest {
            selection: Some(selection.clone()),
        })
        .await
        .unwrap();
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let input = runtime
        .submit_input(input_request(&session_id, "question"))
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &input.run.run_id).await;
    assert!(
        runtime
            .store
            .get_model_fixed_config(selection)
            .await
            .unwrap()
            .is_none()
    );
    factory.offline.store(true, Ordering::SeqCst);
    let fetches = factory.fetches.load(Ordering::SeqCst);
    runtime
        .generate_session_title(assistant_protocol::GenerateSessionTitleRequest {
            session_id: session_id.clone(),
        })
        .await
        .unwrap();
    let session = runtime.session_for_test(&session_id).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while session
            .lock_state()
            .unwrap()
            .active_title_generation
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.lock_state().unwrap().title, "复用模型");
    assert!(
        runtime
            .goal_submission(&session, assistant_protocol::SubmitInputMode::StartGoal)
            .await
            .is_ok()
    );
    assert!(
        runtime
            .compile_session_compactor(&session, None)
            .await
            .is_ok()
    );
    assert_eq!(factory.fetches.load(Ordering::SeqCst), fetches);
    let requests = model.take_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].tools[0].name.as_str(), "submit_session_title");

    // 模拟进程恢复后仅有二元引用、没有固定参数，不用目录查询补救。
    session.lock_state().unwrap().model_binding = None;
    assert!(
        runtime
            .generate_session_title(assistant_protocol::GenerateSessionTitleRequest {
                session_id: session_id.clone()
            })
            .await
            .is_err()
    );
    assert!(
        runtime
            .compile_session_compactor(&session, None)
            .await
            .is_err()
    );
    assert!(
        runtime
            .goal_submission(&session, assistant_protocol::SubmitInputMode::StartGoal)
            .await
            .is_err()
    );
    assert_eq!(factory.fetches.load(Ordering::SeqCst), fetches);
    assert_eq!(session.lock_state().unwrap().title, "复用模型");
}

#[tokio::test]
async fn manual_model_is_offline_selectable_and_origin_is_immutable() {
    use assistant_protocol::ModelConfigOrigin;
    let factory = Arc::new(DiscoveryFactory::default());
    let runtime = fixture(factory.clone());
    let mut selection = create(&runtime).await;
    selection.model_id = "manual-only".into();
    factory.offline.store(true, Ordering::SeqCst);
    let before = factory.fetches.load(Ordering::SeqCst);
    let draft = runtime
        .get_model_configuration(assistant_protocol::GetModelConfigurationRequest {
            selection: selection.clone(),
            origin: ModelConfigOrigin::Manual,
        })
        .await
        .unwrap();
    assert_eq!(draft.origin, ModelConfigOrigin::Manual);
    runtime
        .save_model_fixed_config(SaveModelFixedConfigRequest {
            selection: selection.clone(),
            parameters: parameters(),
            origin: ModelConfigOrigin::Manual,
        })
        .await
        .unwrap();
    runtime
        .set_default_model(assistant_protocol::SetDefaultModelRequest {
            selection: Some(selection.clone()),
        })
        .await
        .unwrap();
    assert!(
        runtime
            .save_model_fixed_config(SaveModelFixedConfigRequest {
                selection: selection.clone(),
                parameters: parameters(),
                origin: ModelConfigOrigin::Online,
            })
            .await
            .is_err()
    );
    let fixed = runtime
        .get_model_configuration(selection.clone().into())
        .await
        .unwrap();
    assert_eq!(fixed.origin, ModelConfigOrigin::Manual);
    assert_eq!(fixed.source, ModelConfigurationSource::Fixed);
    assert_eq!(factory.fetches.load(Ordering::SeqCst), before);
    runtime
        .reset_model_fixed_config(selection.clone())
        .await
        .unwrap();
    assert!(
        runtime
            .get_model_configuration(selection.into())
            .await
            .is_err()
    );
}
