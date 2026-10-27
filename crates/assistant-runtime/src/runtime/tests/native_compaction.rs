use super::*;
use crate::StoredInputState;
use agent_model::{ContextPolicy, ModelCompactionFuture};
use agent_types::{
    AssistantMessagePurpose, OpaqueProviderState, ProtocolId, ProviderContextEffect,
};
use assistant_protocol::{RunStatus, ShutdownRuntimeRequest};
use std::sync::atomic::{AtomicUsize, Ordering};

struct NativeFactory {
    script: Arc<ScriptedModelService>,
    compactions: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}
struct NativeModel {
    policy: ContextPolicy,
    script: Arc<ScriptedModelService>,
    compactions: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}
impl ModelServiceFactory for NativeFactory {
    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError> {
        use sha2::{Digest, Sha256};
        let scope = format!(
            "{:x}",
            Sha256::digest(request.provider_state_binding.unwrap_or("unbound"))
        );
        Ok(ModelServiceBundle::text_only(Arc::new(NativeModel {
            policy: if request.context_management_mode
                == agent_model::ContextManagementMode::LocalSummary
            {
                ContextPolicy::LocalSummary
            } else {
                ContextPolicy::ServerCompaction {
                    threshold_tokens: request.compaction_threshold_tokens,
                    state_scope_fingerprint: scope,
                }
            },
            script: self.script.clone(),
            compactions: self.compactions.clone(),
            requests: self.requests.clone(),
        })))
    }
}

#[tokio::test]
async fn refreshed_compression_parameters_continue_existing_session_without_reselection() {
    for initially_native in [false, true] {
        let factory = Arc::new(NativeFactory {
            script: Arc::new(ScriptedModelService::new(
                model_capabilities(false),
                8192,
                (0..3).map(|i| {
                    ModelScript::Events(message_events(&assistant_text_with_usage(
                        &format!("response-{i}"),
                        "continued",
                        120,
                    )))
                }),
            )),
            compactions: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let runtime = runtime_with_factories(
            factory.clone(),
            Arc::new(StaticSystemPromptFactory),
            ToolSetSnapshot::default(),
            64,
        );
        let mut provider = model_fixture::provider("synthetic");
        provider.connection.provider_type = assistant_protocol::ProviderType::Openai;
        provider.connection.protocol_preference =
            assistant_protocol::ProviderProtocolPreference::Responses;
        provider.server_history_key = Some("configuration-scope".into());
        runtime.store.put_provider(provider).await.unwrap();
        let mut parameters = model_fixture::parameters();
        parameters.server_compaction_enabled = initially_native;
        parameters.server_compaction_support = assistant_protocol::ModelFeatureSupport::Supported;
        model_fixture::save_fixed(&runtime, "fixture", parameters.clone()).await;
        runtime.restore_model_settings().await.unwrap();
        let id = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        submit_completed_turn(&runtime, &id, "before parameter refresh").await;
        let before = runtime.store.load_conversation(&id).await.unwrap();
        parameters.server_compaction_enabled = !initially_native;
        model_fixture::save_fixed(&runtime, "fixture", parameters).await;
        for _ in 0..2 {
            submit_completed_turn(&runtime, &id, "continue existing history").await;
            runtime
                .session(&id)
                .await
                .unwrap()
                .ensure_healthy()
                .unwrap();
        }
        let after = runtime.store.load_conversation(&id).await.unwrap();
        assert_eq!(
            &after.messages[..before.messages.len()],
            before.messages.as_slice()
        );
        assert_eq!(
            factory.compactions.load(Ordering::SeqCst),
            0,
            "switching does not create a handoff compression request"
        );
        let loaded = runtime.store.load_session_state(&id).await.unwrap();
        assert!(
            loaded
                .state
                .runs
                .iter()
                .all(|run| run.status == RunStatus::Completed)
        );
        assert!(
            loaded
                .state
                .inputs
                .iter()
                .all(|input| input.state == StoredInputState::Committed)
        );
        assert_eq!(
            loaded.state.sessions[0].context_management_mode,
            Some(if initially_native {
                agent_model::ContextManagementMode::LocalSummary
            } else {
                agent_model::ContextManagementMode::ServerCompaction
            })
        );
        runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .unwrap();
    }
}
impl ModelService for NativeModel {
    fn context_policy(&self) -> &ContextPolicy {
        &self.policy
    }
    fn capabilities(&self) -> &ModelCapabilities {
        self.script.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        8192
    }
    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        self.requests.lock().unwrap().push(request.clone());
        self.script.stream(request, context)
    }
    fn compact(
        &self,
        request: ModelRequest,
        context: ModelCallContext,
    ) -> ModelCompactionFuture<'_> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            self.requests.lock().unwrap().push(request);
            let index = self.compactions.fetch_add(1, Ordering::SeqCst);
            let ContextPolicy::ServerCompaction {
                state_scope_fingerprint,
                ..
            } = &self.policy
            else {
                unreachable!()
            };
            let mut message = assistant_text_with_usage(&format!("native-{index}"), "", 30);
            message.purpose = AssistantMessagePurpose::ContextCompaction;
            message.parts = vec![AssistantPart::ProviderState(
                OpaqueProviderState::new_routed(
                    ProviderId::new("fixture").unwrap(),
                    ProtocolId::new("responses").unwrap(),
                    "responses.compacted_window",
                    "application/json",
                    1,
                    None,
                    state_scope_fingerprint,
                    b"{}".to_vec(),
                )
                .unwrap()
                .with_context_effect(ProviderContextEffect::ReplacePriorContext)
                .unwrap(),
            )];
            Ok(message)
        })
    }
}

#[tokio::test]
async fn native_to_local_keeps_cutoff_through_manual_or_automatic_summary_and_restart() {
    for automatic in [false, true] {
        let mut scripts: Vec<_> = (0..4)
            .map(|i| {
                ModelScript::Events(message_events(&assistant_text_with_usage(
                    &format!("response-{i}"),
                    &"reliable response ".repeat(100),
                    120,
                )))
            })
            .collect();
        if automatic {
            scripts.push(ModelScript::FailEstablishment(
                ModelError::ContextOverflow {
                    message: "synthetic local overflow".into(),
                },
            ));
        }
        scripts.extend([
            ModelScript::Events(message_events(&assistant_text("summary", "retained task"))),
            ModelScript::Events(message_events(&assistant_text("continued", "done"))),
            ModelScript::Events(message_events(&assistant_text("restarted", "done again"))),
        ]);
        let factory = Arc::new(NativeFactory {
            script: Arc::new(ScriptedModelService::new(
                model_capabilities(false),
                8192,
                scripts,
            )),
            compactions: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let runtime = runtime_with_factories(
            factory.clone(),
            Arc::new(StaticSystemPromptFactory),
            ToolSetSnapshot::default(),
            64,
        );
        let mut provider = model_fixture::provider("synthetic");
        provider.connection.provider_type = assistant_protocol::ProviderType::Openai;
        provider.connection.protocol_preference =
            assistant_protocol::ProviderProtocolPreference::Responses;
        provider.server_history_key = Some("native-switch-config".into());
        runtime.store.put_provider(provider).await.unwrap();
        let mut parameters = model_fixture::parameters();
        parameters.server_compaction_enabled = true;
        parameters.server_compaction_support = assistant_protocol::ModelFeatureSupport::Supported;
        model_fixture::save_fixed(&runtime, "fixture", parameters.clone()).await;
        runtime.restore_model_settings().await.unwrap();
        let id = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        submit_completed_turn(&runtime, &id, "discarded task one").await;
        submit_completed_turn(&runtime, &id, "discarded task two").await;
        runtime
            .compact_session(assistant_protocol::CompactSessionRequest {
                session_id: id.clone(),
                operation_id: assistant_protocol::IdempotencyKey::new("native-before-switch")
                    .unwrap(),
                expected_generation: 1,
            })
            .await
            .unwrap();
        let old_history = runtime.store.load_conversation(&id).await.unwrap();
        submit_completed_turn(&runtime, &id, "after native one").await;
        submit_completed_turn(&runtime, &id, "after native two").await;
        parameters.server_compaction_enabled = false;
        model_fixture::save_fixed(&runtime, "fixture", parameters).await;
        let switch_request = factory.requests.lock().unwrap().len();
        if !automatic {
            let result = runtime
                .compact_session(assistant_protocol::CompactSessionRequest {
                    session_id: id.clone(),
                    operation_id: assistant_protocol::IdempotencyKey::new("local-after-switch")
                        .unwrap(),
                    expected_generation: 2,
                })
                .await
                .unwrap();
            assert!(matches!(
                result.outcome,
                assistant_protocol::CompactSessionOutcome::Compacted { .. }
            ));
        }
        submit_completed_turn(&runtime, &id, "continue locally").await;
        let store = runtime.store.clone();
        runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .unwrap();
        // 只复用测试内存 Store；重新打开正式 Runtime，不能借旧 Journal 掩盖恢复错误。
        let reopened = AssistantRuntime::open(
            RuntimeConfig::new(NonZeroUsize::new(64).unwrap()),
            Arc::new(MissingConfigSource),
            factory.clone(),
            Arc::new(StaticSystemPromptFactory),
            static_run_tool_factory(ToolSetSnapshot::default()),
            Arc::new(TestChildWorkspaceFactory::default()),
            store,
            Arc::new(crate::permission::VolatilePermissionFileStore::default()),
        )
        .await
        .unwrap();
        reopened
            .config_registry
            .replace_document_for_test(TEST_CONFIG);
        reopened.restore_model_settings().await.unwrap();
        submit_completed_turn(&reopened, &id, "continue after restart").await;
        let history = reopened.store.load_conversation(&id).await.unwrap();
        assert_eq!(
            &history.messages[..old_history.messages.len()],
            old_history.messages.as_slice()
        );
        assert_eq!(factory.compactions.load(Ordering::SeqCst), 1);
        {
            let requests = factory.requests.lock().unwrap();
            let local_requests = &requests[switch_request..];
            assert!(local_requests.len() >= 3);
            for request in local_requests {
                assert!(
                    !request
                        .conversation
                        .messages
                        .iter()
                        .any(|message| old_history.messages.contains(message))
                );
                request.conversation.validate_tool_exchange_pairs().unwrap();
            }
            assert!(
                local_requests
                    .last()
                    .unwrap()
                    .conversation
                    .messages
                    .iter()
                    .any(|message| matches!(message, ConversationMessage::ContextSummary(_)))
            );
        }
        reopened
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn native_window_survives_manual_generation_switch_model_change_and_journal_reload() {
    let factory = Arc::new(NativeFactory {
        script: Arc::new(ScriptedModelService::new(
            model_capabilities(false),
            8192,
            [
                ModelScript::FailEstablishment(ModelError::ContextOverflow {
                    message: "synthetic overflow".into(),
                }),
                ModelScript::Events(message_events(&assistant_text_with_usage(
                    "normal-one",
                    "first",
                    120,
                ))),
                ModelScript::Events(message_events(&assistant_text_with_usage(
                    "normal-two",
                    "second",
                    130,
                ))),
                ModelScript::Events(message_events(&assistant_text_with_usage(
                    "normal-three",
                    "third",
                    140,
                ))),
            ],
        )),
        compactions: Arc::new(AtomicUsize::new(0)),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        64,
    );
    let mut provider = model_fixture::provider("synthetic");
    provider.connection.provider_type = assistant_protocol::ProviderType::Openai;
    provider.connection.protocol_preference =
        assistant_protocol::ProviderProtocolPreference::Responses;
    provider.server_history_key = Some("native-provider-config".into());
    runtime.store.put_provider(provider).await.unwrap();
    let mut parameters = model_fixture::parameters();
    parameters.server_compaction_enabled = true;
    parameters.server_compaction_support = assistant_protocol::ModelFeatureSupport::Supported;
    model_fixture::save_fixed(&runtime, "fixture", parameters.clone()).await;
    model_fixture::save_fixed(&runtime, "new-model", parameters).await;
    runtime.restore_model_settings().await.unwrap();
    let id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    submit_completed_turn(&runtime, &id, "original task").await;
    let session = runtime.session(&id).await.unwrap();
    assert_eq!(session.lock_state().unwrap().body_generation, 1);
    let first_history = runtime.store.load_conversation(&id).await.unwrap();
    assert!(
        first_history
            .messages
            .iter()
            .any(|m| matches!(m, ConversationMessage::User(_)))
    );
    assert_eq!(factory.compactions.load(Ordering::SeqCst), 1);
    submit_completed_turn(&runtime, &id, "second task").await;
    let before = runtime.store.load_conversation(&id).await.unwrap();
    let compact = runtime
        .compact_session(assistant_protocol::CompactSessionRequest {
            session_id: id.clone(),
            operation_id: assistant_protocol::IdempotencyKey::new("native-manual").unwrap(),
            expected_generation: 1,
        })
        .await
        .unwrap();
    assert!(matches!(
        compact.outcome,
        assistant_protocol::CompactSessionOutcome::Compacted {
            result_generation: 2,
            ..
        }
    ));
    let after = runtime.store.load_conversation(&id).await.unwrap();
    assert_eq!(&after.messages[..before.messages.len()], &before.messages);
    assert_eq!(after.messages.len(), before.messages.len() + 1);
    assert_eq!(factory.compactions.load(Ordering::SeqCst), 2);
    let pending_view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        pending_view
            .snapshot
            .value
            .usage
            .context
            .unwrap()
            .calibration_pending,
        Some(true)
    );
    // 计量只在内容/绑定仍一致时为精确；下一次可靠正常响应使旧测量失效。
    {
        let mut state = session.lock_state().unwrap();
        let binding = state.server_history_key.clone();
        state.context_measurements.insert(
            String::new(),
            crate::session::ContextMeasurement {
                history_hash: crate::session::context_history_hash(&after).unwrap(),
                session_binding: binding,
                input_tokens: 42,
            },
        );
    }
    let measured_view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    let measured = measured_view.snapshot.value.usage.context.unwrap();
    assert_eq!(measured.calibration_pending, None);
    assert_eq!(
        measured.accuracy,
        assistant_protocol::ContextUsageAccuracy::Exact
    );
    // 显式换模旋转临时续接身份并重载 Journal，但不能丢弃已可靠提交的压缩窗口。
    let old_binding = session.lock_state().unwrap().server_history_key.clone();
    runtime
        .set_session_model(assistant_protocol::SetSessionModelRequest {
            session_id: id.clone(),
            model_selection: Some(test_model_selection("new-model")),
        })
        .await
        .unwrap();
    assert_ne!(
        session.lock_state().unwrap().server_history_key,
        old_binding
    );
    session.lock_state().unwrap().journal = None;
    submit_completed_turn(&runtime, &id, "after model change and reload").await;
    {
        let requests = factory.requests.lock().unwrap();
        let final_request = requests.last().unwrap();
        assert!(
            matches!(final_request.conversation.messages.first(), Some(ConversationMessage::Assistant(m)) if m.id.as_str() == "native-1")
        );
        assert!(!final_request.conversation.messages.iter().any(
            |m| matches!(m, ConversationMessage::Assistant(m) if m.id.as_str() == "normal-one")
        ));
    }
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: id.clone(),
        })
        .await
        .unwrap();
    assert!(!view.snapshot.value.conversation.items.iter().any(|item| matches!(item, assistant_protocol::ConversationItem::Assistant(m) if m.message_id.as_str().starts_with("native-"))));
    assert_eq!(
        view.snapshot
            .value
            .usage
            .accumulated
            .as_ref()
            .unwrap()
            .total_tokens,
        Some(450)
    );
    assert_eq!(
        view.snapshot.value.usage.context.unwrap().accuracy,
        assistant_protocol::ContextUsageAccuracy::Estimated
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn native_child_compacts_twice_with_its_own_scope_and_keeps_parent_history_separate() {
    let mut delegate = assistant_text("delegate", "");
    delegate.finish_reason = FinishReason::ToolCalls;
    delegate.parts = vec![AssistantPart::ToolCall(ToolCall {
        id: ToolCallId::new("delegate-call").unwrap(),
        name: ToolName::new("delegate_task").unwrap(),
        arguments: json!({"title":"child", "task":"child work"}),
    })];
    let mut tool = assistant_text("child-tool", "");
    tool.finish_reason = FinishReason::ToolCalls;
    tool.parts = vec![AssistantPart::ToolCall(ToolCall {
        id: ToolCallId::new("child-tool-call").unwrap(),
        name: ToolName::new("load_skill").unwrap(),
        arguments: json!({"name":"missing-synthetic-skill"}),
    })];
    let factory = Arc::new(NativeFactory {
        script: Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [
                ModelScript::Events(message_events(&delegate)),
                ModelScript::FailEstablishment(ModelError::ContextOverflow {
                    message: "child overflow one".into(),
                }),
                ModelScript::Events(message_events(&tool)),
                ModelScript::FailEstablishment(ModelError::ContextOverflow {
                    message: "child overflow two".into(),
                }),
                ModelScript::Events(message_events(&assistant_text_with_usage(
                    "child-final",
                    "child done",
                    120,
                ))),
                ModelScript::Events(message_events(&assistant_text_with_usage(
                    "parent-final",
                    "parent done",
                    140,
                ))),
            ],
        )),
        compactions: Arc::new(AtomicUsize::new(0)),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        64,
    );
    runtime.config_registry.replace_document_for_test(&format!("{TEST_CONFIG}\n[agent.defaults.delegation]\nmax_tasks_per_run=1\nmax_concurrent_tasks=1\ntask_timeout_ms=10000\nmax_steps=20\nmax_tool_calls=20\nmax_output_tokens=1024\n"));
    let mut provider = model_fixture::provider("synthetic");
    provider.connection.provider_type = assistant_protocol::ProviderType::Openai;
    provider.connection.protocol_preference =
        assistant_protocol::ProviderProtocolPreference::Responses;
    provider.server_history_key = Some("native-child-provider".into());
    runtime.store.put_provider(provider).await.unwrap();
    let mut parameters = model_fixture::parameters();
    parameters.server_compaction_enabled = true;
    parameters.server_compaction_support = assistant_protocol::ModelFeatureSupport::Supported;
    model_fixture::save_fixed(&runtime, "fixture", parameters).await;
    runtime.restore_model_settings().await.unwrap();
    let id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    set_auto_approval(&runtime, &id).await;
    submit_completed_turn(&runtime, &id, "delegate child work").await;
    let recovered = runtime.store.load_runtime().await.unwrap();
    assert_eq!(recovered.child_history.len(), 1);
    let child = &recovered.child_history[0].1;
    assert_eq!(
        child.status,
        Some(assistant_protocol::ChildTaskStatus::Completed),
        "{:?}",
        child.error
    );
    assert_eq!(child.body_generation, 1);
    assert_eq!(factory.compactions.load(Ordering::SeqCst), 2);
    let child_history = runtime
        .store
        .load_child_conversation(&id, &child.child_task_id)
        .await
        .unwrap();
    assert_eq!(child_history.messages.len(), 6);
    assert_eq!(child.message_count, 6);
    let parent_history = runtime.store.load_conversation(&id).await.unwrap();
    assert!(
        !parent_history.messages.iter().any(
            |m| matches!(m, ConversationMessage::Assistant(m) if !m.purpose.is_model_response())
        )
    );
    let child_fingerprints: Vec<_> = child_history
        .messages
        .iter()
        .filter_map(|m| match m {
            ConversationMessage::Assistant(m) if !m.purpose.is_model_response() => {
                match &m.parts[0] {
                    AssistantPart::ProviderState(state) => state.route_fingerprint(),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(child_fingerprints.len(), 2);
    assert_eq!(child_fingerprints[0], child_fingerprints[1]);
    let session = runtime.session(&id).await.unwrap();
    let (parent_compactor, prepared) = runtime
        .compile_session_compactor(&session, None)
        .await
        .unwrap();
    let parent_replacement = parent_compactor
        .compact_manual(
            parent_history.clone(),
            parent_history,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let crate::context_compaction::ManualCompactionCandidate::Replacement { conversation, .. } =
        parent_replacement
    else {
        panic!("native candidate");
    };
    let ConversationMessage::Assistant(parent_state) = &conversation.messages[0] else {
        panic!("native state");
    };
    let AssistantPart::ProviderState(parent_state) = &parent_state.parts[0] else {
        panic!("native state");
    };
    assert_ne!(
        parent_state.route_fingerprint(),
        Some(child_fingerprints[0])
    );
    assert_eq!(
        prepared.model.context_management_mode(),
        agent_model::ContextManagementMode::ServerCompaction
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .unwrap();
}
