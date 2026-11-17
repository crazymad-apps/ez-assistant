use super::*;

use crate::StagedAttachmentUpload;
use assistant_protocol::{
    AgentVariant, ApprovalMode, ArchiveSessionRequest, DeleteSessionRequest, ForkSessionRequest,
    GenerateSessionTitleRequest, IdempotencyKey, ListAttachmentsRequest,
    ListConversationPageRequest, ListRunsRequest, PrepareDeleteSessionRequest,
    ReenterFromUserMessageRequest, RenameSessionRequest, RestoreSessionRequest, SessionLifecycle,
    SessionListFilter, SessionTitleOrigin, SetSessionApprovalModeRequest, SetSessionModelRequest,
    SetSessionPinnedRequest, SetSessionVariantRequest,
};

#[tokio::test]
async fn automatic_and_manual_title_triggers_share_the_same_side_path() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("answer-title", "answer"))),
            ModelScript::Events(message_events(&assistant_title_tool_call(
                "automatic-title",
                "automatic-title-call",
                "## “自动标题”",
            ))),
            ModelScript::Events(message_events(&assistant_title_tool_call(
                "manual-title",
                "manual-title-call",
                "手动标题",
            ))),
        ],
    ));
    let runtime = runtime(model.clone());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    runtime
        .session(&session.session_id)
        .await
        .expect("session controller")
        .lock_state()
        .expect("session state")
        .automatic_title_pending = true;
    let submitted = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            session_id: session.session_id.clone(),
            message: "请实现标题旁路".to_owned(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("input");
    wait_for_terminal(&runtime, &session.session_id, &submitted.run.run_id).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime
                .get_session(GetSessionRequest {
                    session_id: session.session_id.clone(),
                })
                .await
                .expect("session")
                .session
                .title
                == "自动标题"
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("automatic title completes");

    runtime
        .generate_session_title(GenerateSessionTitleRequest {
            session_id: session.session_id.clone(),
        })
        .await
        .expect("manual title accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if runtime
                .get_session(GetSessionRequest {
                    session_id: session.session_id.clone(),
                })
                .await
                .expect("session")
                .session
                .title
                == "手动标题"
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("manual title completes");
    let usage = runtime
        .store
        .get_session_usage(&session.session_id)
        .await
        .expect("usage");
    assert_eq!(usage.auxiliary_request_count, 2);
    let requests = model.take_requests();
    assert_eq!(requests[1].generation.max_output_tokens, Some(1_024));
    assert_eq!(requests[2].generation.max_output_tokens, Some(1_024));
    assert!(requests[1].reasoning.is_none());
    assert_eq!(requests[1].tools.len(), 1);
    assert_eq!(requests[1].tools[0].name.as_str(), "submit_session_title");
    assert_eq!(
        requests[1].tool_choice,
        ToolChoice::Named(ToolName::new("submit_session_title").expect("tool name"))
    );
}

#[tokio::test]
async fn manual_title_failure_publishes_a_safe_model_diagnostic() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("answer", "answer"))),
            ModelScript::FailEstablishment(agent_model::ModelError::Provider {
                message: "sanitized fixture rejection".to_owned(),
                status: Some(400),
            }),
        ],
    ));
    let runtime = runtime(model);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session
        .session_id;
    let submitted = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            session_id: session_id.clone(),
            message: "需要一个可重命名的会话".to_owned(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("input");
    wait_for_terminal(&runtime, &session_id, &submitted.run.run_id).await;

    let mut events = runtime.subscribe_events();
    runtime
        .generate_session_title(GenerateSessionTitleRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("manual title accepted");
    let (outcome, error) = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let RuntimeEvent::SessionTitleGenerationFinished {
                session_id: event_session_id,
                outcome,
                error,
                ..
            } = events.recv().await.expect("title event")
                && event_session_id == session_id
            {
                break (outcome, error);
            }
        }
    })
    .await
    .expect("title failure event");
    assert_eq!(
        outcome,
        assistant_protocol::SessionTitleGenerationFinishedOutcome::Failed
    );
    let error = error.expect("safe title error");
    assert_eq!(
        error.code,
        assistant_protocol::RuntimeErrorCode::ModelExecutionFailed
    );
    assert_eq!(
        error.message,
        "model execution failed (kind=provider_rejected)"
    );
}

#[tokio::test]
async fn plain_text_title_continuation_never_overwrites_the_fallback_title() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("answer", "已完成调研"))),
            ModelScript::Events(message_events(&assistant_text(
                "title-continuation",
                "调研完成，更新计划如下。",
            ))),
        ],
    ));
    let runtime = runtime(model);
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    let controller = runtime
        .session(&session.session_id)
        .await
        .expect("session controller");
    controller
        .lock_state()
        .expect("session state")
        .automatic_title_pending = true;
    let submitted = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            session_id: session.session_id.clone(),
            message: "分析 computer use 实现".to_owned(),
            variant: AgentVariant::Build,
            mode: assistant_protocol::SubmitInputMode::Normal,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("input");
    wait_for_terminal(&runtime, &session.session_id, &submitted.run.run_id).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let settled = {
                let state = controller.lock_state().expect("session state");
                state.active_title_generation.is_none() && !state.automatic_title_pending
            };
            if settled {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("automatic title settles");

    assert_eq!(
        runtime
            .get_session(GetSessionRequest {
                session_id: session.session_id,
            })
            .await
            .expect("session")
            .session
            .title,
        "分析 computer use 实现"
    );
}

#[tokio::test]
async fn fork_creates_an_independent_session_at_a_reliable_assistant_message() {
    let assistant = assistant_text("assistant-fork-point", "fork here");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant)),
            ModelScript::Events(message_events(&assistant_text(
                "assistant-fork-continued",
                "independent continuation",
            ))),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
    )
    .await;
    let source = runtime
        .create_session(CreateSessionRequest {
            title: Some("Source".to_owned()),
            model_selection: None,
            workspace_id: None,
        })
        .await
        .expect("source session")
        .session;
    let submitted = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: source.session_id.clone(),
            message: "first turn".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("submit source input");
    wait_for_terminal(&runtime, &source.session_id, &submitted.run.run_id).await;
    let page = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: source.session_id.clone(),
            },
            cursor: None,
            turn_limit: Some(30),
        })
        .await
        .expect("source page")
        .snapshot
        .value;

    let forked = runtime
        .fork_session(ForkSessionRequest {
            session_id: source.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-fork-point")
                .expect("fork point"),
            expected_generation: page.generation,
        })
        .await
        .expect("fork session")
        .session;

    assert_ne!(forked.session_id, source.session_id);
    assert_eq!(forked.title, "Source（分支）");
    assert_eq!(forked.model_selection, source.model_selection);
    assert_eq!(forked.message_count, 2);
    let history = store
        .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
            session_id: forked.session_id.clone(),
        })
        .await
        .unwrap()
        .facts;
    assert_eq!(
        history.runs.len(),
        1,
        "Fork must retain reliable Run history"
    );
    assert_eq!(history.inputs.len(), 1);
    assert_eq!(history.message_origins.len(), 2);
    assert!(history.runs[0].inherited && !history.runs[0].partial);
    assert_eq!(history.runs[0].run_id, submitted.run.run_id);
    assert!(
        history
            .message_origins
            .iter()
            .all(|origin| origin.inherited)
    );
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: forked.session_id.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert!(view.active_run.is_none() && view.runs.is_empty());
    let assistant_protocol::ConversationItem::Assistant(projected) = &view.conversation.items[1]
    else {
        panic!("inherited complete Assistant");
    };
    assert!(projected.inherited && !projected.partial);
    assert_eq!(projected.run_id.as_ref(), Some(&submitted.run.run_id));
    assert_eq!(
        projected.status,
        Some(assistant_protocol::RunStatus::Completed)
    );
    // 源消息 ID 确实位于可靠继承历史；分配只修改尚未提交的新身份，不写历史或恢复控制。
    let mut collided = history.inputs[0].user_message_id.clone();
    {
        let target = runtime.session(&forked.session_id).await.unwrap();
        let state = target.lock_state().unwrap();
        crate::run::ensure_message_id_available(&mut collided, &state, &history).unwrap();
    }
    assert_ne!(collided, history.inputs[0].user_message_id);
    assert!(
        !history
            .message_origins
            .iter()
            .any(|origin| origin.message_id == collided)
    );
    let forked_conversation = runtime
        .conversation_snapshot(&forked.session_id)
        .await
        .expect("forked conversation");
    assert_eq!(forked_conversation.messages.len(), 2);
    assert_eq!(
        runtime
            .conversation_snapshot(&source.session_id)
            .await
            .expect("source conversation")
            .messages
            .len(),
        2
    );

    let repeated = runtime
        .fork_session(ForkSessionRequest {
            session_id: forked.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-fork-point").unwrap(),
            expected_generation: 1,
        })
        .await
        .expect("repeat fork of inherited history")
        .session;
    for target in [&forked.session_id, &repeated.session_id] {
        assert!(matches!(
            runtime
                .retry_run(assistant_protocol::RetryRunRequest {
                    session_id: target.clone(),
                    run_id: submitted.run.run_id.clone(),
                })
                .await,
            Err(RuntimeError::RunNotRetryable { .. })
        ));
        assert!(
            runtime
                .cancel_run(CancelRunRequest {
                    session_id: target.clone(),
                    run_id: submitted.run.run_id.clone(),
                })
                .await
                .is_err()
        );
        assert!(matches!(
            runtime
                .reenter_from_user_message({
                    let (expected_generation, expected_message_count) =
                        edit_basis(&runtime, &target.clone()).await;
                    ReenterFromUserMessageRequest {
                        session_id: target.clone(),
                        message_id: assistant_protocol::MessageId::new(
                            history.inputs[0].user_message_id.as_str(),
                        )
                        .unwrap(),
                        message: "cannot rewrite inherited execution".to_owned(),
                        attachment_ids: Vec::new(),
                        idempotency_key: assistant_protocol::IdempotencyKey::new(
                            crate::id::generate("edit-test").unwrap(),
                        )
                        .unwrap(),
                        variant: AgentVariant::Build,

                        expected_generation,
                        expected_message_count,
                        quotes: Vec::new(),
                        skill_edit: assistant_protocol::UserMessageSkillEdit::Retain,
                        mcp_edit: assistant_protocol::UserMessageMcpEdit::Retain,
                    }
                })
                .await,
            Err(RuntimeError::UserMessageNotEditable)
        ));
    }
    assert_eq!(model.take_requests().len(), 1, "Fork never runs a model");
    let confirmation = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: source.session_id.clone(),
        })
        .await
        .unwrap();
    runtime
        .delete_session(DeleteSessionRequest {
            session_id: source.session_id,
            confirmation_token: confirmation.confirmation_token,
        })
        .await
        .unwrap();
    drop(runtime);
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
    )
    .await;
    for target in [&forked.session_id, &repeated.session_id] {
        assert_eq!(
            store
                .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                    session_id: target.clone(),
                })
                .await
                .unwrap()
                .facts,
            history,
            "source deletion and restart preserve each owner's facts"
        );
    }
    let continuation = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: forked.session_id.clone(),
            message: "continue independently".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .unwrap();
    assert_ne!(continuation.run.run_id, submitted.run.run_id);
    assert_eq!(
        wait_for_terminal(&runtime, &forked.session_id, &continuation.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let continued = store
        .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
            session_id: forked.session_id,
        })
        .await
        .unwrap();
    assert_eq!(continued.facts.runs.len(), 2);
    assert!(
        continued
            .facts
            .runs
            .iter()
            .any(|run| { run.run_id == continuation.run.run_id && !run.inherited && !run.partial })
    );
    assert_eq!(model.take_requests().len(), 1);
}

#[tokio::test]
async fn delete_requires_a_bound_one_time_confirmation_and_removes_only_the_target_session() {
    let runtime = runtime(empty_model());
    let target = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("target")
        .session;
    let other = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("other")
        .session;
    let mismatched = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: target.session_id.clone(),
        })
        .await
        .expect("prepare mismatched deletion");
    assert!(matches!(
        runtime
            .delete_session(DeleteSessionRequest {
                session_id: other.session_id.clone(),
                confirmation_token: mismatched.confirmation_token.clone(),
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    assert!(matches!(
        runtime
            .delete_session(DeleteSessionRequest {
                session_id: target.session_id.clone(),
                confirmation_token: mismatched.confirmation_token,
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));

    let prepared = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: target.session_id.clone(),
        })
        .await
        .expect("prepare deletion");
    assert_eq!(prepared.impact.message_count, 0);
    runtime
        .delete_session(DeleteSessionRequest {
            session_id: target.session_id.clone(),
            confirmation_token: prepared.confirmation_token,
        })
        .await
        .expect("delete target");

    assert!(matches!(
        runtime
            .get_session(assistant_protocol::GetSessionRequest {
                session_id: target.session_id,
            })
            .await,
        Err(RuntimeError::SessionNotFound { .. })
    ));
    assert!(
        runtime
            .get_session(assistant_protocol::GetSessionRequest {
                session_id: other.session_id,
            })
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn faulted_idle_session_can_fork_reliable_history_and_be_deleted() {
    let assistant = assistant_text("assistant-faulted-fork", "reliable prefix");
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [ModelScript::Events(message_events(&assistant))],
    ));
    let runtime = runtime(model);
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("source session")
        .session;
    let submitted = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: source.session_id.clone(),
            message: "persist a reliable turn".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("submit input");
    wait_for_terminal(&runtime, &source.session_id, &submitted.run.run_id).await;
    runtime
        .session(&source.session_id)
        .await
        .expect("source controller")
        .mark_faulted()
        .expect("fault source");
    let generation = runtime
        .list_conversation_page(ListConversationPageRequest {
            owner: assistant_protocol::ConversationOwner::MainSession {
                session_id: source.session_id.clone(),
            },
            cursor: None,
            turn_limit: Some(30),
        })
        .await
        .expect("reliable source conversation")
        .snapshot
        .value
        .generation;

    let forked = runtime
        .fork_session(ForkSessionRequest {
            session_id: source.session_id.clone(),
            fork_point: assistant_protocol::MessageId::new("assistant-faulted-fork")
                .expect("fork point"),
            expected_generation: generation,
        })
        .await
        .expect("fork faulted source");
    assert_eq!(forked.session.message_count, 2);

    let prepared = runtime
        .prepare_delete_session(PrepareDeleteSessionRequest {
            session_id: source.session_id.clone(),
        })
        .await
        .expect("prepare faulted source deletion");
    runtime
        .delete_session(DeleteSessionRequest {
            session_id: source.session_id.clone(),
            confirmation_token: prepared.confirmation_token,
        })
        .await
        .expect("delete faulted source");
    assert!(matches!(
        runtime
            .get_session(assistant_protocol::GetSessionRequest {
                session_id: source.session_id,
            })
            .await,
        Err(RuntimeError::SessionNotFound { .. })
    ));
}

#[tokio::test]
async fn first_input_generates_a_bounded_title_without_overwriting_a_user_title() {
    let runtime = runtime(empty_model());
    let generated = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("generated session")
        .session;
    runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: generated.session_id.clone(),
            message: "\n  首行作为会话标题  \n后续正文".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("first input");
    let generated_summary = runtime
        .get_session(assistant_protocol::GetSessionRequest {
            session_id: generated.session_id,
        })
        .await
        .expect("generated summary")
        .session;
    assert_eq!(generated_summary.title, "首行作为会话标题");
    assert_eq!(
        generated_summary.title_origin,
        SessionTitleOrigin::Generated
    );

    let renamed = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("renamed session")
        .session;
    runtime
        .rename_session(RenameSessionRequest {
            session_id: renamed.session_id.clone(),
            title: "  用户标题  ".to_owned(),
        })
        .await
        .expect("rename");
    runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: renamed.session_id.clone(),
            message: "不应覆盖标题".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
            variant: AgentVariant::Build,
        })
        .await
        .expect("first renamed input");
    let renamed_summary = runtime
        .get_session(assistant_protocol::GetSessionRequest {
            session_id: renamed.session_id,
        })
        .await
        .expect("renamed summary")
        .session;
    assert_eq!(renamed_summary.title, "用户标题");
    assert_eq!(renamed_summary.title_origin, SessionTitleOrigin::User);
}

#[tokio::test]
async fn pin_is_idempotent_and_does_not_change_global_creation_order() {
    let runtime = runtime(empty_model());
    let first = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first")
        .session;
    let second = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second")
        .session;
    let activity_time = first.updated_at_ms;
    let before = runtime
        .list_sessions(ListSessionsRequest::default())
        .await
        .unwrap()
        .sessions
        .into_iter()
        .map(|s| s.session_id)
        .collect::<Vec<_>>();
    for _ in 0..2 {
        runtime
            .set_session_pinned(SetSessionPinnedRequest {
                session_id: first.session_id.clone(),
                is_pinned: true,
            })
            .await
            .expect("pin");
    }
    let listed = runtime
        .list_sessions(ListSessionsRequest::default())
        .await
        .expect("sessions")
        .sessions;
    assert_eq!(
        listed
            .iter()
            .map(|s| s.session_id.clone())
            .collect::<Vec<_>>(),
        before
    );
    let pinned = listed
        .iter()
        .find(|s| s.session_id == first.session_id)
        .unwrap();
    assert!(pinned.is_pinned);
    assert_eq!(pinned.updated_at_ms, activity_time);
    assert!(listed.iter().any(|s| s.session_id == second.session_id));
    let snapshot = runtime
        .get_application_snapshot(assistant_protocol::GetApplicationSnapshotRequest::default())
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(snapshot.pinned_sessions.len(), 1);
    assert_eq!(snapshot.pinned_sessions[0].session_id, first.session_id);
}

#[tokio::test]
async fn title_changes_do_not_advance_session_activity_time() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    let activity_time = created.updated_at_ms;

    let renamed = runtime
        .rename_session(RenameSessionRequest {
            session_id: created.session_id,
            title: "Only metadata changed".to_owned(),
        })
        .await
        .expect("rename");

    assert_eq!(renamed.session.updated_at_ms, activity_time);
}

#[tokio::test]
async fn faulted_session_still_allows_non_execution_metadata_changes() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session")
        .session;
    runtime
        .session(&created.session_id)
        .await
        .expect("controller")
        .mark_faulted()
        .expect("fault session");

    let renamed = runtime
        .rename_session(RenameSessionRequest {
            session_id: created.session_id.clone(),
            title: "故障后仍可编辑".to_owned(),
        })
        .await
        .expect("rename faulted session");
    assert_eq!(renamed.session.title, "故障后仍可编辑");

    let pinned = runtime
        .set_session_pinned(SetSessionPinnedRequest {
            session_id: created.session_id,
            is_pinned: true,
        })
        .await
        .expect("pin faulted session");
    assert!(pinned.session.is_pinned);
}

#[tokio::test]
async fn archived_session_is_filtered_read_only_and_can_be_restored() {
    let runtime = runtime(empty_model());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;

    let archived = runtime
        .archive_session(ArchiveSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("archive");
    assert_eq!(archived.session.lifecycle, SessionLifecycle::Archived);
    assert!(
        runtime
            .list_sessions(ListSessionsRequest::default())
            .await
            .expect("active sessions")
            .sessions
            .is_empty()
    );
    assert_eq!(
        runtime
            .list_sessions(ListSessionsRequest {
                filter: SessionListFilter::Archived,
                ..Default::default()
            })
            .await
            .expect("archived sessions")
            .sessions
            .len(),
        1
    );
    assert!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("archived conversation")
            .messages
            .is_empty()
    );
    assert!(
        runtime
            .list_runs(ListRunsRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("archived runs")
            .runs
            .is_empty()
    );
    assert!(matches!(
        runtime
            .submit_started_input(SubmitInputRequest {
                delivery: Default::default(),
                mode: assistant_protocol::SubmitInputMode::Normal,
                variant: assistant_protocol::AgentVariant::Build,
                session_id: session_id.clone(),
                message: "not allowed".to_owned(),
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::SessionArchived { .. })
    ));

    let restored = runtime
        .restore_session(RestoreSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("restore");
    assert_eq!(restored.session.lifecycle, SessionLifecycle::Active);
    assert_eq!(
        runtime
            .list_sessions(ListSessionsRequest::default())
            .await
            .expect("active sessions")
            .sessions[0]
            .session_id,
        session_id
    );
}

#[tokio::test]
async fn model_switch_changes_only_the_reference_and_requires_an_idle_active_session() {
    let runtime = runtime(empty_model());
    model_fixture::save_fixed(&runtime, "alternate", model_fixture::parameters()).await;
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;
    let prompt = runtime
        .session_for_test(&session_id)
        .await
        .current_system_prompt()
        .expect("system prompt")
        .clone();

    let changed = runtime
        .set_session_model(SetSessionModelRequest {
            session_id: session_id.clone(),
            model_selection: Some(test_model_selection("alternate")),
        })
        .await
        .expect("change model");
    assert_eq!(
        changed.session.model_selection,
        Some(test_model_selection("alternate"))
    );
    assert_eq!(
        runtime
            .session_for_test(&session_id)
            .await
            .current_system_prompt()
            .expect("system prompt"),
        prompt
    );

    runtime
        .archive_session(ArchiveSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("archive");
    assert!(matches!(
        runtime
            .set_session_model(SetSessionModelRequest {
                session_id,
                model_selection: Some(test_model_selection("fixture")),
            })
            .await,
        Err(RuntimeError::SessionArchived { .. })
    ));
}

#[tokio::test]
async fn active_run_blocks_archive_model_switch_and_history_reentry() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let runtime = hanging_runtime(1, None, entered.clone(), cleanup.clone());
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;
    let active = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "active input".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("active run");
    let pending = wait_for_pending_approval(&runtime, &session_id).await;
    runtime
        .decide_approval(assistant_protocol::DecideApprovalRequest {
            session_id: session_id.clone(),
            approval_id: pending.approval_id,
            decision: assistant_protocol::ApprovalDecision::AllowOnce,
        })
        .await
        .expect("allow the active tool once");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("tool entered");

    let variant = runtime
        .set_session_variant(SetSessionVariantRequest {
            session_id: session_id.clone(),
            variant: AgentVariant::Plan,
        })
        .await
        .expect("variant changes during an active run");
    assert_eq!(variant.session.current_variant, AgentVariant::Plan);
    let approval = runtime
        .set_session_approval_mode(SetSessionApprovalModeRequest {
            session_id: session_id.clone(),
            approval_mode: ApprovalMode::Auto,
        })
        .await
        .expect("approval mode changes during an active run");
    assert_eq!(approval.session.approval_mode, ApprovalMode::Auto);
    let frozen = runtime
        .get_run(GetRunRequest {
            session_id: session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("active run snapshot")
        .run;
    assert_eq!(frozen.variant, AgentVariant::Build);
    assert_eq!(frozen.approval_mode, ApprovalMode::Ask);

    assert!(matches!(
        runtime
            .archive_session(ArchiveSessionRequest {
                session_id: session_id.clone(),
            })
            .await,
        Err(RuntimeError::SessionNotIdle { .. })
    ));
    assert!(matches!(
        runtime
            .set_session_model(SetSessionModelRequest {
                session_id: session_id.clone(),
                model_selection: Some(test_model_selection("fixture")),
            })
            .await,
        Err(RuntimeError::SessionNotIdle { .. })
    ));
    assert!(matches!(
        runtime
            .reenter_from_user_message({
                let (expected_generation, expected_message_count) =
                    edit_basis(&runtime, &session_id.clone()).await;
                ReenterFromUserMessageRequest {
                    variant: assistant_protocol::AgentVariant::Build,
                    session_id: session_id.clone(),
                    message_id: assistant_protocol::MessageId::new("unknown").expect("message id"),
                    message: "replacement".to_owned(),
                    attachment_ids: Vec::new(),
                    idempotency_key: assistant_protocol::IdempotencyKey::new(
                        crate::id::generate("edit-test").unwrap(),
                    )
                    .unwrap(),

                    expected_generation,
                    expected_message_count,
                    quotes: Vec::new(),
                    skill_edit: assistant_protocol::UserMessageSkillEdit::Retain,
                    mcp_edit: assistant_protocol::UserMessageMcpEdit::Retain,
                }
            })
            .await,
        Err(RuntimeError::SessionNotIdle { .. })
    ));

    runtime
        .cancel_run(CancelRunRequest {
            session_id: session_id.clone(),
            run_id: active.run.run_id.clone(),
        })
        .await
        .expect("cancel active");
    tokio::time::timeout(Duration::from_secs(1), cleanup.notified())
        .await
        .expect("tool cleanup");
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &active.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
}

#[tokio::test]
async fn reenter_from_user_destroys_the_target_and_tail_without_creating_a_branch() {
    use assistant_protocol::{
        GetUserMessageEditRequest, UserMessageEditPreparation, UserMessageMcpEdit,
        UserMessageSkillEdit,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("a-1", "first answer"))),
            ModelScript::Events(message_events(&assistant_text("a-2", "second answer"))),
            ModelScript::Events(message_events(&assistant_text("a-3", "replacement answer"))),
        ],
    ));
    let factory = Arc::new(FailOnceModelFactory::new(model));
    factory.should_fail.store(false, Ordering::SeqCst);
    let runtime = runtime_with_factories(
        factory.clone(),
        Arc::new(StaticSystemPromptFactory),
        ToolSetSnapshot::default(),
        32,
    );
    let created = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("session");
    let session_id = created.session.session_id;
    let old_attachment = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: session_id.clone(),
            original_name: "old.txt".to_owned(),
            staging_path: "/volatile/old.part".to_owned(),
            blob_hash: "e".repeat(64),
            size_bytes: 10,
            media_type: None,
        })
        .await
        .expect("old attachment")
        .attachment;
    let replacement_attachment = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: session_id.clone(),
            original_name: "replacement.txt".to_owned(),
            staging_path: "/volatile/replacement.part".to_owned(),
            blob_hash: "f".repeat(64),
            size_bytes: 20,
            media_type: None,
        })
        .await
        .expect("replacement attachment")
        .attachment;
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "first question".to_owned(),
            attachment_ids: vec![old_attachment.attachment_id.clone()],
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("first input");
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    let first_message = runtime
        .conversation_snapshot(&session_id)
        .await
        .unwrap()
        .messages
        .into_iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => {
                Some(assistant_protocol::MessageId::new(user.id.as_str()).unwrap())
            }
            _ => None,
        })
        .unwrap();
    let prepare = || async {
        let result = runtime
            .get_user_message_edit(GetUserMessageEditRequest {
                session_id: session_id.clone(),
                message_id: first_message.clone(),
            })
            .await
            .expect("read edit preparation");
        let UserMessageEditPreparation::Ready {
            message,
            variant,
            generation,
            message_count,
        } = result.snapshot.value
        else {
            panic!("original user input must be editable");
        };
        assert_eq!(
            message.attachment_ids.as_slice(),
            std::slice::from_ref(&old_attachment.attachment_id)
        );
        assert_eq!(variant, AgentVariant::Build);
        ReenterFromUserMessageRequest {
            session_id: session_id.clone(),
            message_id: first_message.clone(),
            message: "replacement question".to_owned(),
            variant: AgentVariant::Plan,
            attachment_ids: vec![replacement_attachment.attachment_id.clone()],
            quotes: message.quotes,
            skill_edit: UserMessageSkillEdit::Retain,
            mcp_edit: UserMessageMcpEdit::Retain,
            expected_generation: generation,
            expected_message_count: message_count,
            idempotency_key: IdempotencyKey::new("replace-1").unwrap(),
        }
    };
    let stale = prepare().await;
    let second = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            variant: assistant_protocol::AgentVariant::Build,
            session_id: session_id.clone(),
            message: "second question".to_owned(),
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: None,
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .expect("second input");
    wait_for_terminal(&runtime, &session_id, &second.run.run_id).await;
    let before = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("conversation before replacement");
    assert!(matches!(
        runtime.reenter_from_user_message(stale).await,
        Err(RuntimeError::SnapshotStale)
    ));
    let request = prepare().await;
    factory.should_fail.store(true, Ordering::SeqCst);
    assert!(
        runtime
            .reenter_from_user_message(request.clone())
            .await
            .is_err()
    );
    assert_eq!(
        runtime.conversation_snapshot(&session_id).await.unwrap(),
        before
    );

    let replacement = runtime
        .reenter_from_user_message(request.clone())
        .await
        .expect("replace history");
    let repeated = runtime
        .reenter_from_user_message(request)
        .await
        .expect("read the committed receipt");
    assert_eq!(repeated.input_id, replacement.input_id);
    assert_eq!(repeated.run.run_id, replacement.run.run_id);
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &replacement.run.run_id,)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let after = runtime
        .conversation_snapshot(&session_id)
        .await
        .expect("conversation after replacement");
    let texts = after
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(user) => user.parts.iter().find_map(|part| match part {
                UserPart::Text(text) => Some(text.text.as_str()),
                UserPart::Injected(_)
                | UserPart::InternalContext(_)
                | UserPart::QuotedText(_)
                | UserPart::FileReferences(_) => None,
            }),
            ConversationMessage::Assistant(assistant) => {
                assistant.parts.iter().find_map(|part| match part {
                    AssistantPart::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(texts, ["replacement question", "replacement answer"]);
    let remaining_files = after
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => user.parts.iter().find_map(|part| match part {
                UserPart::FileReferences(files) => Some(&files.files),
                UserPart::Text(_)
                | UserPart::Injected(_)
                | UserPart::InternalContext(_)
                | UserPart::QuotedText(_) => None,
            }),
            _ => None,
        })
        .expect("replacement file references");
    assert_eq!(remaining_files.len(), 1);
    assert_eq!(
        remaining_files[0].readable_path,
        replacement_attachment.agent_readable_path
    );
    assert_eq!(
        runtime
            .list_attachments(ListAttachmentsRequest {
                session_id: session_id.clone(),
            })
            .await
            .expect("session attachments")
            .attachments
            .len(),
        2
    );
    let runs = runtime
        .list_runs(ListRunsRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("runs");
    assert_eq!(runs.runs.len(), 1);
    assert_eq!(runs.runs[0].run_id, replacement.run.run_id);
    assert!(
        runtime
            .get_run(GetRunRequest {
                session_id: session_id.clone(),
                run_id: first.run.run_id,
            })
            .await
            .is_err()
    );
    let session = runtime
        .get_session(GetSessionRequest { session_id })
        .await
        .expect("session")
        .session;
    assert_eq!(session.message_count, 2);
    assert_eq!(session.current_variant, AgentVariant::Build);
}

#[tokio::test]
async fn unchanged_edit_resends_with_frozen_skill_after_catalog_disables_it() {
    use assistant_protocol::{
        GetUserMessageEditRequest, UserMessageEditPreparation, UserMessageMcpEdit,
        UserMessageSkillEdit,
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8_192,
        [
            ModelScript::Events(message_events(&assistant_text("skill-initial", "done"))),
            ModelScript::Events(message_events(&assistant_text(
                "skill-edited",
                "done again",
            ))),
        ],
    ));
    let mut runtime = runtime(model.clone());
    runtime.skill_package_source = Arc::new(super::sessions::StaticSkillPackageSource);
    let session_id = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let first = runtime
        .submit_started_input(SubmitInputRequest {
            delivery: Default::default(),
            mode: assistant_protocol::SubmitInputMode::Normal,
            session_id: session_id.clone(),
            message: "review first draft".to_owned(),
            variant: AgentVariant::Build,
            attachment_ids: Vec::new(),
            quotes: Vec::new(),
            skill_name: Some("review".to_owned()),
            mcp_server_key: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    wait_for_terminal(&runtime, &session_id, &first.run.run_id).await;
    let body = runtime.conversation_snapshot(&session_id).await.unwrap();
    let original = body
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => Some(user),
            _ => None,
        })
        .unwrap();
    runtime
        .set_skill_enabled(assistant_protocol::SetSkillEnabledRequest {
            workspace_id: None,
            name: "review".to_owned(),
            enabled: false,
        })
        .await
        .unwrap();

    let message_id = assistant_protocol::MessageId::new(original.id.as_str()).unwrap();
    let preparation = runtime
        .get_user_message_edit(GetUserMessageEditRequest {
            session_id: session_id.clone(),
            message_id: message_id.clone(),
        })
        .await
        .unwrap();
    let UserMessageEditPreparation::Ready {
        message,
        variant,
        generation,
        message_count,
    } = preparation.snapshot.value
    else {
        panic!("frozen skill can be recovered");
    };
    assert_eq!(message.skill.as_ref().unwrap().name, "review");
    let request = ReenterFromUserMessageRequest {
        session_id: session_id.clone(),
        message_id,
        message: message.text,
        variant,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        expected_generation: generation,
        expected_message_count: message_count,
        idempotency_key: IdempotencyKey::new("edit-frozen-skill").unwrap(),
        skill_edit: UserMessageSkillEdit::Retain,
        mcp_edit: UserMessageMcpEdit::Retain,
    };
    assert!(
        runtime
            .reenter_from_user_message(ReenterFromUserMessageRequest {
                skill_edit: UserMessageSkillEdit::Select {
                    name: "review".to_owned()
                },
                ..request.clone()
            })
            .await
            .is_err()
    );
    let edited = runtime.reenter_from_user_message(request).await.unwrap();
    assert_ne!(edited.run.run_id, first.run.run_id);
    assert_ne!(edited.user_message_id.as_str(), original.id.as_str());
    assert_eq!(
        wait_for_terminal(&runtime, &session_id, &edited.run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    let frozen = |index: usize| {
        requests[index]
            .conversation
            .messages
            .iter()
            .find_map(|message| match message {
                ConversationMessage::User(user) => user.parts.iter().find_map(|part| match part {
                    UserPart::InternalContext(part) if part.kind == "skill_activation" => {
                        Some(part)
                    }
                    _ => None,
                }),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(frozen(0).text, frozen(1).text);
    assert_ne!(frozen(0).boundary_id, frozen(1).boundary_id);
}
