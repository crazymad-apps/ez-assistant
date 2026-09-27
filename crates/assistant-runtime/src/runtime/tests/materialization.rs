use super::*;

type RecordedRequirements = Vec<(String, Vec<String>, Vec<String>)>;

struct MutableAgentDirectory {
    state: Arc<Mutex<Option<Vec<assistant_protocol::CustomAgentConfig>>>>,
    requested: Arc<Mutex<RecordedRequirements>>,
}

impl crate::CustomAgentDirectorySource for MutableAgentDirectory {
    fn load(
        &self,
    ) -> futures_util::future::BoxFuture<'_, crate::RuntimeResult<crate::CenterCustomAgentDirectory>>
    {
        Box::pin(async move {
            let agents = self
                .state
                .lock()
                .unwrap()
                .clone()
                .ok_or(RuntimeError::ConfigurationUnavailable)?;
            Ok(crate::CenterCustomAgentDirectory {
                center_id: "center-test".into(),
                agents,
            })
        })
    }

    fn resolve_plugin(
        &self,
        center_id: &str,
        name: &str,
        required_skills: Vec<String>,
        required_mcp: Vec<String>,
    ) -> futures_util::future::BoxFuture<
        '_,
        crate::RuntimeResult<assistant_protocol::PluginPackageSelection>,
    > {
        let center_id = center_id.to_owned();
        let name = name.to_owned();
        Box::pin(async move {
            self.requested
                .lock()
                .unwrap()
                .push((name.clone(), required_skills, required_mcp));
            Ok(assistant_protocol::PluginPackageSelection {
                name,
                version: "1.0.0".into(),
                sha256: "a".repeat(64),
                size_bytes: 7,
                format: assistant_protocol::PluginArchiveFormat::TarGzip,
                origin: assistant_protocol::PluginPackageOrigin::Center { center_id },
                expected_active_sha256: None,
            })
        })
    }
}

fn manifest(message: &str) -> assistant_protocol::SessionMaterializationManifest {
    assistant_protocol::SessionMaterializationManifest {
        custom_agent: None,
        idempotency_key: assistant_protocol::IdempotencyKey::new("materialization-runtime-key")
            .expect("key"),
        workspace_id: None,
        model_selection: None,
        reasoning_effort: None,
        variant: assistant_protocol::AgentVariant::Build,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        message: message.to_owned(),
        mode: assistant_protocol::SubmitInputMode::StartGoal,
        attachments: vec![
            assistant_protocol::SessionMaterializationAttachment {
                selection_key: "selection-one".to_owned(),
                original_name: "one.txt".to_owned(),
                size_bytes: 3,
            },
            assistant_protocol::SessionMaterializationAttachment {
                selection_key: "selection-two".to_owned(),
                original_name: "two.txt".to_owned(),
                size_bytes: 3,
            },
        ],
        quotes: vec![assistant_protocol::QuotedTextSnapshot {
            quote_id: assistant_protocol::PartId::new("quote-materialized").expect("quote id"),
            exact: "selected text".to_owned(),
            prefix: "before".to_owned(),
            suffix: "after".to_owned(),
            source_owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: assistant_protocol::SessionId::new("source-session")
                    .expect("source session id"),
            },
            source_generation: 1,
            source_message_id: assistant_protocol::MessageId::new("source-message")
                .expect("source message id"),
            text_start_utf16: 0,
            text_end_utf16: 13,
            source_role: assistant_protocol::QuotedTextSourceRoleSnapshot::Assistant,
            source_label: "source session".to_owned(),
            source_created_at_ms: Some(123),
            source_available: true,
        }],
        skill_name: Some("review".to_owned()),
        mcp_server_key: None,
    }
}

fn staged() -> Vec<StagedSessionAttachment> {
    vec![
        StagedSessionAttachment {
            selection_key: "selection-one".to_owned(),
            original_name: "one.txt".to_owned(),
            staging_path: "/volatile/one.part".to_owned(),
            blob_hash: "1".repeat(64),
            size_bytes: 3,
            media_type: Some("text/plain".to_owned()),
        },
        StagedSessionAttachment {
            selection_key: "selection-two".to_owned(),
            original_name: "two.txt".to_owned(),
            staging_path: "/volatile/two.part".to_owned(),
            blob_hash: "2".repeat(64),
            size_bytes: 3,
            media_type: Some("text/plain".to_owned()),
        },
    ]
}

#[tokio::test]
async fn center_agent_catalog_refreshes_and_resolves_missing_plugin_requirements() {
    use assistant_protocol::{
        AgentCapabilityReference, AgentDependencyStatus, CustomAgentConfig, CustomAgentSelection,
        CustomAgentSource, PrepareCustomAgentRequest,
    };
    let original = CustomAgentConfig {
        name: "库房 CAD 绘图助手".into(),
        description: "仓储平面图".into(),
        prompt: "旧版规则".into(),
        skills: vec![AgentCapabilityReference {
            name: "compact-shelving-cad".into(),
            plugin: Some("warehouse-cad-guidance".into()),
        }],
        mcp: vec![AgentCapabilityReference {
            name: "autocad-ai".into(),
            plugin: Some("warehouse-cad-guidance".into()),
        }],
    };
    let state = Arc::new(Mutex::new(Some(vec![original.clone()])));
    let requested = Arc::new(Mutex::new(Vec::new()));
    let source = MutableAgentDirectory {
        state: state.clone(),
        requested: requested.clone(),
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default())
        .with_custom_agent_source(Arc::new(source));
    let selection = CustomAgentSelection {
        name: original.name.clone(),
        source: CustomAgentSource::Center {
            center_id: "center-test".into(),
        },
    };
    let prepared = runtime
        .prepare_custom_agent(PrepareCustomAgentRequest {
            selection: selection.clone(),
            workspace_id: None,
        })
        .await
        .expect("prepare center agent");
    assert_eq!(prepared.dependencies.len(), 2);
    assert!(
        prepared
            .dependencies
            .iter()
            .all(|item| item.status == AgentDependencyStatus::PluginMissing)
    );
    assert_eq!(prepared.install_candidates.len(), 1);
    assert_eq!(
        requested.lock().unwrap().as_slice(),
        &[(
            "warehouse-cad-guidance".into(),
            vec!["compact-shelving-cad".into()],
            vec!["autocad-ai".into()],
        )]
    );
    let mut blocked = manifest("review");
    blocked.attachments.clear();
    blocked.quotes.clear();
    blocked.skill_name = None;
    blocked.mode = assistant_protocol::SubmitInputMode::Normal;
    blocked.custom_agent = Some(selection.clone());
    assert!(matches!(
        runtime.materialize_session(blocked, Vec::new()).await,
        Err(RuntimeError::ConfigurationUnavailable)
    ));
    *state.lock().unwrap() = Some(vec![CustomAgentConfig {
        prompt: "新版规则".into(),
        ..original
    }]);
    let listed = runtime.list_custom_agents().await.unwrap();
    assert!(!listed.center_unavailable);
    assert_eq!(listed.agents[0].config.prompt, "新版规则");
    *state.lock().unwrap() = None;
    let offline = runtime.list_custom_agents().await.unwrap();
    assert!(offline.center_unavailable);
    assert_eq!(offline.agents[0].config.prompt, "新版规则");
    assert!(runtime.resolve_custom_agent(&selection).await.is_err());
}

#[tokio::test]
async fn center_agent_missing_capability_can_send_and_retry_with_frozen_snapshot() {
    use assistant_protocol::{CustomAgentConfig, CustomAgentSelection, CustomAgentSource};
    let config = CustomAgentConfig {
        name: "企业审阅助手".into(),
        description: "审阅".into(),
        prompt: "企业原始规则".into(),
        skills: Vec::new(),
        mcp: vec![assistant_protocol::AgentCapabilityReference {
            name: "unavailable-cad".into(),
            plugin: None,
        }],
    };
    let state = Arc::new(Mutex::new(Some(vec![config.clone()])));
    let source = MutableAgentDirectory {
        state: state.clone(),
        requested: Arc::new(Mutex::new(Vec::new())),
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "center-reply",
            "done",
        )))],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default())
        .with_custom_agent_source(Arc::new(source));
    let mut request = manifest("review");
    request.idempotency_key =
        assistant_protocol::IdempotencyKey::new("center-agent-retry").unwrap();
    request.mode = assistant_protocol::SubmitInputMode::Normal;
    request.attachments.clear();
    request.quotes.clear();
    request.skill_name = None;
    request.custom_agent = Some(CustomAgentSelection {
        name: config.name,
        source: CustomAgentSource::Center {
            center_id: "center-test".into(),
        },
    });
    let prepared = runtime
        .prepare_custom_agent(assistant_protocol::PrepareCustomAgentRequest {
            selection: request.custom_agent.clone().unwrap(),
            workspace_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        prepared.dependencies[0].status,
        assistant_protocol::AgentDependencyStatus::CapabilityMissing
    );
    let first = runtime
        .materialize_session(request.clone(), Vec::new())
        .await
        .unwrap();
    wait_for_terminal(&runtime, &first.session.session_id, &first.run.run_id).await;
    let session = runtime.session(&first.session.session_id).await.unwrap();
    assert!(
        session
            .current_system_prompt()
            .unwrap()
            .parts()
            .iter()
            .any(|part| part.contains("unavailable-cad")
                && part.contains("Availability is not guaranteed"))
    );
    *state.lock().unwrap() = None;
    let retry = runtime
        .materialize_session(request.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(retry.session.session_id, first.session.session_id);
    let mut different_selection = request;
    different_selection.custom_agent = None;
    assert!(
        runtime
            .materialize_session(different_selection, Vec::new())
            .await
            .is_err()
    );
    assert!(
        runtime
            .session(&first.session.session_id)
            .await
            .unwrap()
            .current_system_prompt()
            .unwrap()
            .parts()
            .iter()
            .any(|part| part == "企业原始规则")
    );
}

#[tokio::test]
async fn personal_agent_first_send_freezes_prompt_across_edit_retry_clear_and_fork() {
    use assistant_protocol::{
        ClearSessionRequest, CustomAgentConfig, CustomAgentSelection, CustomAgentSource,
        ForkSessionRequest, SavePersonalAgentRequest,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "agent-reply",
            "done",
        )))],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    let original = CustomAgentConfig {
        name: "审阅助手".into(),
        description: "审阅".into(),
        prompt: "按原始规范审阅。".into(),
        skills: Vec::new(),
        mcp: Vec::new(),
    };
    let saved = runtime
        .save_personal_agent(SavePersonalAgentRequest {
            expected_revision: None,
            config: original.clone(),
        })
        .await
        .expect("save agent");
    let mut request = manifest("review the patch");
    request.idempotency_key = assistant_protocol::IdempotencyKey::new("agent-materialize").unwrap();
    request.mode = assistant_protocol::SubmitInputMode::Normal;
    request.attachments.clear();
    request.quotes.clear();
    request.skill_name = None;
    request.custom_agent = Some(CustomAgentSelection {
        name: original.name.clone(),
        source: CustomAgentSource::Personal,
    });
    let first = runtime
        .materialize_session(request.clone(), Vec::new())
        .await
        .expect("first send");
    assert_eq!(first.session.custom_agent, request.custom_agent);
    wait_for_terminal(&runtime, &first.session.session_id, &first.run.run_id).await;
    let session = runtime.session(&first.session.session_id).await.unwrap();
    assert!(
        session
            .current_system_prompt()
            .unwrap()
            .parts()
            .iter()
            .any(|part| part == &original.prompt)
    );
    let frozen = session.custom_agent().cloned().unwrap();
    let edited = CustomAgentConfig {
        prompt: "新规范，不能影响旧会话。".into(),
        ..original
    };
    runtime
        .save_personal_agent(SavePersonalAgentRequest {
            expected_revision: Some(saved.revision),
            config: edited.clone(),
        })
        .await
        .expect("edit agent");
    let retry = runtime
        .materialize_session(request, Vec::new())
        .await
        .expect("same key retry");
    assert_eq!(retry.session.session_id, first.session.session_id);
    assert_eq!(
        runtime
            .session(&first.session.session_id)
            .await
            .unwrap()
            .custom_agent(),
        Some(&frozen)
    );

    let conversation = runtime
        .conversation_snapshot(&first.session.session_id)
        .await
        .unwrap();
    let assistant_id = conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::Assistant(message) => Some(message.id.clone()),
            _ => None,
        })
        .expect("assistant reply");
    let generation = session.lock_state().unwrap().body_generation;
    let fork = runtime
        .fork_session(ForkSessionRequest {
            session_id: first.session.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new(assistant_id.as_str()).unwrap(),
            expected_generation: generation,
        })
        .await
        .expect("fork");
    assert_eq!(fork.session.custom_agent, first.session.custom_agent);
    assert_eq!(
        runtime
            .session(&fork.session.session_id)
            .await
            .unwrap()
            .custom_agent(),
        Some(&frozen)
    );

    runtime
        .clear_session(ClearSessionRequest {
            session_id: first.session.session_id.clone(),
            expected_generation: generation,
            operation_id: assistant_protocol::IdempotencyKey::new("clear-frozen-agent").unwrap(),
        })
        .await
        .expect("clear");
    let cleared = runtime.session(&first.session.session_id).await.unwrap();
    assert_eq!(cleared.custom_agent(), Some(&frozen));
    assert!(
        cleared
            .current_system_prompt()
            .unwrap()
            .parts()
            .iter()
            .any(|part| part == &frozen.config.prompt)
    );
    assert!(
        !cleared
            .current_system_prompt()
            .unwrap()
            .parts()
            .iter()
            .any(|part| part == &edited.prompt)
    );
}

#[tokio::test]
async fn text_only_materialization_creates_no_provisional_attachment_or_session() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant_text(
            "materialized-answer",
            "done",
        )))],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    assert!(
        runtime
            .materialize_session(manifest("invalid file batch"), Vec::new())
            .await
            .is_err()
    );
    assert!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .await
            .expect("invalid materialization leaves no session")
            .sessions
            .is_empty()
    );
    let mut request = manifest("text only");
    request.idempotency_key =
        assistant_protocol::IdempotencyKey::new("text-only-materialization").expect("key");
    request.mode = assistant_protocol::SubmitInputMode::Normal;
    request.attachments.clear();
    request.quotes.clear();
    request.skill_name = None;

    let result = runtime
        .materialize_session(request, Vec::new())
        .await
        .expect("materialize text-only session");
    assert!(result.attachments.is_empty());
    assert_eq!(result.session.title, "text only");
    assert_eq!(
        wait_for_terminal(&runtime, &result.session.session_id, &result.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .await
            .expect("list text-only session")
            .sessions
            .len(),
        1
    );
}

#[tokio::test]
async fn first_send_materializes_goal_skill_quotes_and_files_once() {
    let entered = Arc::new(Notify::new());
    let model = Arc::new(CancellationAwareModel {
        capabilities: model_capabilities(true),
        entered: entered.clone(),
    });
    let mut runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    runtime.skill_package_source = Arc::new(super::sessions::StaticSkillPackageSource);

    let first = runtime
        .materialize_session(manifest("ship this change"), staged())
        .await
        .expect("materialize first send");
    assert_eq!(first.session.title, "ship this change");
    assert_eq!(first.attachments.len(), 2);
    assert_eq!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .await
            .expect("list sessions")
            .sessions
            .len(),
        1
    );

    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("materialized run starts");
    let conversation = runtime
        .conversation_snapshot(&first.session.session_id)
        .await
        .expect("materialized conversation");
    let user = conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => Some(user),
            _ => None,
        })
        .expect("materialized user message");
    assert_eq!(
        user.parts
            .iter()
            .filter(|part| matches!(part, UserPart::QuotedText(_)))
            .count(),
        1
    );
    assert!(
        user.parts
            .iter()
            .any(|part| { matches!(part, UserPart::QuotedText(quote) if !quote.source_available) })
    );
    assert_eq!(
        user.parts
            .iter()
            .filter_map(|part| match part {
                UserPart::FileReferences(files) => Some(files.files.len()),
                _ => None,
            })
            .sum::<usize>(),
        2
    );
    assert!(user.parts.iter().any(|part| {
        matches!(part, UserPart::InternalContext(context) if context.kind.starts_with("goal_"))
    }));
    assert!(user.parts.iter().any(|part| {
        matches!(part, UserPart::InternalContext(context) if context.kind == "skill_activation")
    }));

    let retry = runtime
        .materialize_session(manifest("ship this change"), staged())
        .await
        .expect("response-loss retry");
    assert_eq!(retry.session.session_id, first.session.session_id);
    assert_eq!(retry.input_id, first.input_id);
    assert_eq!(retry.run.run_id, first.run.run_id);
    assert_eq!(
        runtime
            .list_sessions(assistant_protocol::ListSessionsRequest::default())
            .await
            .expect("list sessions after retry")
            .sessions
            .len(),
        1
    );

    let conflict = runtime
        .materialize_session(manifest("different content"), staged())
        .await
        .expect_err("same key with different manifest must conflict");
    assert_eq!(
        conflict.to_protocol_info().code,
        assistant_protocol::RuntimeErrorCode::Conflict
    );
    let (goal_id, generation) = {
        let session = runtime
            .session(&first.session.session_id)
            .await
            .expect("materialized session");
        let state = session.lock_state().expect("session state");
        let goal = state.goal.as_ref().expect("materialized Goal");
        (goal.id.clone(), goal.generation)
    };
    runtime
        .stop_goal(assistant_protocol::StopGoalRequest {
            session_id: first.session.session_id,
            goal_id,
            expected_generation: generation,
        })
        .await
        .expect("stop materialized Goal");
}
