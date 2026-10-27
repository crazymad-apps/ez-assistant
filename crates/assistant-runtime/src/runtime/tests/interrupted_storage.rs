use super::*;
use agent_core::ExchangeReceipt;
use assistant_protocol::{ChildTaskId, ChildTaskStatus, ToolCallId as ProtocolToolCallId};

enum ChildFixture {
    Accepted,
    PendingTool,
    Completed,
    MissingFinal,
}

#[tokio::test]
async fn prepare_interrupts_unstarted_child_without_starting_queued_work() {
    recover(ChildFixture::Accepted).await;
}

#[tokio::test]
async fn prepare_closes_started_and_unstarted_child_tools_without_replaying_them() {
    recover(ChildFixture::PendingTool).await;
}

#[tokio::test]
async fn prepare_reads_completed_child_file_to_close_parent_exchange() {
    recover(ChildFixture::Completed).await;
}

#[tokio::test]
async fn prepare_keeps_parent_pending_when_completed_child_has_no_final_fact() {
    recover(ChildFixture::MissingFinal).await;
}

async fn recover(fixture: ChildFixture) {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8_192,
        [],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let config = RuntimeConfig::new(NonZeroUsize::new(256).unwrap());
    let first = runtime_with_store(model.clone(), store.clone(), config).await;
    let session_id = first
        .create_session(CreateSessionRequest {
            title: Some("Interrupted storage fixture".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .session
        .session_id;
    let controller = first.session_for_test(&session_id).await;
    controller.lock_state().unwrap().queue_paused_by_user = true;
    let request = |message: &str| SubmitInputRequest {
        mode: assistant_protocol::SubmitInputMode::Normal,
        variant: assistant_protocol::AgentVariant::Build,
        session_id: session_id.clone(),
        message: message.into(),
        attachment_ids: vec![],
        quotes: vec![],
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    };
    let accepted = first
        .submit_claimed_input(request("interrupted input"))
        .await
        .unwrap();
    let queued = first
        .submit_input(request("must remain queued"))
        .await
        .unwrap();
    let queued_before = store
        .load_runtime()
        .await
        .unwrap()
        .inputs
        .into_iter()
        .find(|input| input.input_id == queued.input_id)
        .unwrap();
    let user = controller.lock_state().unwrap().inputs[&accepted.input_id]
        .stored
        .queued_message
        .clone()
        .unwrap();
    let at_ms = accepted.run.created_at_ms.unwrap();
    store
        .commit_user_message(crate::UserMessageCommit {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            shell: None,
            operation_id: "begin-source".into(),
            session_id: session_id.clone(),
            input_id: accepted.input_id.clone(),
            run_id: accepted.run.run_id.clone(),
            message: Some(user),
            reasoning_effort: None,
            created_at_ms: at_ms,
        })
        .await
        .unwrap();
    let parent_receipt = ExchangeReceipt::new("parent-exchange").unwrap();
    store
        .begin_tool_exchange(crate::PendingToolExchange {
            receipt: parent_receipt.clone(),
            session_id: session_id.clone(),
            run_id: accepted.run.run_id.clone(),
            step: 1,
            assistant: assistant_tool_call("parent-pending", crate::DELEGATE_TASK_TOOL_NAME),
            created_at_ms: at_ms,
        })
        .await
        .unwrap();
    store
        .mark_tool_execution_started(crate::ToolExecutionStart {
            receipt: parent_receipt,
            session_id: session_id.clone(),
            run_id: accepted.run.run_id.clone(),
            call_id: ProtocolToolCallId::new("call-1").unwrap(),
            started_at_ms: at_ms,
        })
        .await
        .unwrap();
    let child_id = ChildTaskId::new("child-recovery").unwrap();
    store
        .create_child_task(crate::NewStoredChildTask {
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            context_window_tokens: 8_192,
            child_task_id: child_id.clone(),
            session_id: session_id.clone(),
            parent_run_id: accepted.run.run_id.clone(),
            parent_tool_call_id: ProtocolToolCallId::new("call-1").unwrap(),
            title: "Saved child".into(),
            system_prompt: agent_model::SystemPromptSnapshot::new(vec!["child".into()]),
            agent_variant: assistant_protocol::AgentVariant::Build,
            created_at_ms: at_ms,
        })
        .await
        .unwrap();
    if !matches!(fixture, ChildFixture::Accepted) {
        store
            .start_child_task(crate::ChildTaskStart {
                operation_id: "start-child".into(),
                child_task_id: child_id.clone(),
                session_id: session_id.clone(),
                message: agent_types::UserMessage {
                    id: MessageId::new("child-user").unwrap(),
                    origin: agent_types::UserMessageOrigin::User,
                    transcript_visibility: agent_types::TranscriptVisibility::Visible,
                    parts: vec![UserPart::Text(TextPart {
                        id: PartId::new("child-text").unwrap(),
                        text: "child input".into(),
                    })],
                },
                started_at_ms: at_ms,
            })
            .await
            .unwrap();
    }
    match fixture {
        ChildFixture::PendingTool => {
            let mut assistant = assistant_tool_call("child-pending", "write_file");
            assistant.parts = ["started", "not-started"].into_iter().map(|id| AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new(id).unwrap(),
                name: ToolName::new("write_file").unwrap(),
                arguments: json!({"path":"/virtual/recovery-fixture","content":"must not execute"}),
            })).collect();
            let receipt = ExchangeReceipt::new("child-exchange").unwrap();
            store
                .begin_child_tool_exchange(crate::PendingChildToolExchange {
                    receipt: receipt.clone(),
                    child_task_id: child_id.clone(),
                    session_id: session_id.clone(),
                    step: 3,
                    assistant,
                    created_at_ms: at_ms,
                })
                .await
                .unwrap();
            store
                .mark_child_tool_execution_started(crate::ChildToolExecutionStart {
                    receipt,
                    child_task_id: child_id.clone(),
                    session_id: session_id.clone(),
                    call_id: ProtocolToolCallId::new("started").unwrap(),
                    started_at_ms: at_ms,
                })
                .await
                .unwrap();
        }
        ChildFixture::Completed | ChildFixture::MissingFinal => {
            let final_message = assistant_text("saved-child-final", "saved child result");
            let complete = matches!(fixture, ChildFixture::Completed);
            store
                .settle_child_task(crate::StoredChildTaskSettlement {
                    operation_id: "saved-child-terminal".into(),
                    child_task_id: child_id.clone(),
                    session_id: session_id.clone(),
                    status: ChildTaskStatus::Completed,
                    cancel_requested: false,
                    error: None,
                    final_message_id: complete.then(|| final_message.id.clone()),
                    messages: if complete {
                        vec![ConversationMessage::Assistant(final_message)]
                    } else {
                        vec![]
                    },
                    finished_at_ms: at_ms,
                })
                .await
                .unwrap();
        }
        ChildFixture::Accepted => {}
    }
    drop(first);
    let restarted = runtime_with_store(model.clone(), store.clone(), config).await;
    let view = restarted
        .get_session_view(GetSessionViewRequest {
            session_id: session_id.clone(),
        })
        .await;
    if matches!(fixture, ChildFixture::MissingFinal) {
        assert!(
            view.is_err(),
            "missing reliable final fact must not become an unknown generic result"
        );
        let Err(RuntimeError::StorageUnavailable {
            source: Some(source),
            ..
        }) = &view
        else {
            panic!("missing final fact must report its storage cause");
        };
        assert_eq!(source.kind(), crate::StoreErrorKind::InvalidData);
        assert!(
            restarted
                .get_session_view(GetSessionViewRequest {
                    session_id: session_id.clone(),
                })
                .await
                .is_err(),
            "repeated preparation must retain the unresolved parent exchange"
        );
        assert!(store.contains_run_control(&accepted.run.run_id).unwrap());
        assert_eq!(
            store
                .load_conversation(&session_id)
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
        assert!(model.take_requests().is_empty());
        return;
    }
    let view = view.unwrap();
    assert!(
        view.snapshot
            .value
            .runs
            .iter()
            .any(|run| run.run_id == accepted.run.run_id
                && run.status == assistant_protocol::RunStatus::Interrupted)
    );
    assert!(!store.contains_run_control(&accepted.run.run_id).unwrap());
    assert!(!store.contains_child_control(&child_id).unwrap());
    let loaded = store.load_runtime().await.unwrap();
    assert_eq!(
        loaded
            .inputs
            .iter()
            .find(|input| input.input_id == queued.input_id),
        Some(&queued_before)
    );
    assert!(loaded.runs.is_empty(), "未领取队列没有执行控制");
    let parent = store.load_conversation(&session_id).await.unwrap();
    parent.validate_tool_exchange_pairs().unwrap();
    assert_eq!(parent.messages.len(), 3);
    let ConversationMessage::Tool(result) = &parent.messages[2] else {
        panic!("parent tool result")
    };
    let child = store
        .load_child_conversation(&session_id, &child_id)
        .await
        .unwrap();
    child.validate_tool_exchange_pairs().unwrap();
    let fact = &loaded.child_history[0].1;
    if matches!(fixture, ChildFixture::Completed) {
        assert_eq!(fact.status, Some(ChildTaskStatus::Completed));
        assert_eq!(fact.finished_at_ms, Some(at_ms));
        assert_eq!(result.result.status, agent_types::ToolResultStatus::Success);
        assert!(format!("{:?}", result.result.content).contains("saved child result"));
    } else {
        assert_eq!(fact.status, Some(ChildTaskStatus::Interrupted));
        assert_eq!(result.result.status, agent_types::ToolResultStatus::Error);
        if matches!(fixture, ChildFixture::Accepted) {
            assert!(child.messages.is_empty());
            assert_eq!(fact.started_at_ms, None);
        } else {
            assert_eq!(child.messages.len(), 4);
            assert!(format!("{:?}", child.messages[2]).contains("outcome is unknown"));
            assert!(format!("{:?}", child.messages[3]).contains("before tool execution started"));
        }
    }
    let before = store
        .load_conversation_history(assistant_protocol::ConversationOwner::ChildTask {
            session_id: session_id.clone(),
            child_task_id: child_id.clone(),
        })
        .await
        .unwrap();
    store.prepare_session_execution(&session_id).await.unwrap();
    assert_eq!(
        store
            .load_child_conversation(&session_id, &child_id)
            .await
            .unwrap(),
        child
    );
    assert_eq!(store.load_conversation(&session_id).await.unwrap(), parent);
    assert_eq!(
        store
            .load_conversation_history(assistant_protocol::ConversationOwner::ChildTask {
                session_id,
                child_task_id: child_id
            })
            .await
            .unwrap(),
        before
    );
    assert!(
        model.take_requests().is_empty(),
        "recovery does not call a model or execute historical tools"
    );
}
