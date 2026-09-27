use super::*;
use assistant_protocol::{
    AnswerFeedbackRequest, CancelFeedbackRequest, FeedbackAnswer, FeedbackRequestSnapshot,
    FeedbackStatus, ListFeedbackRequestsRequest, SubmitInputMode,
};

fn question(mode: &str) -> AssistantMessage {
    let mut message = assistant_tool_call("question-message", "ask_user");
    if let AssistantPart::ToolCall(call) = &mut message.parts[0] {
        call.arguments = json!({"mode":mode,"questions":[{"question":"选哪种格式？","options":[{"label":"PDF"},{"label":"文档"}]}]});
    }
    message
}
fn input(session_id: &SessionId, text: &str) -> SubmitInputRequest {
    SubmitInputRequest {
        session_id: session_id.clone(),
        message: text.into(),
        variant: assistant_protocol::AgentVariant::Build,
        mode: SubmitInputMode::Normal,
        attachment_ids: vec![],
        quotes: vec![],
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    }
}
async fn pending(runtime: &AssistantRuntime, session_id: &SessionId) -> FeedbackRequestSnapshot {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let requests = runtime
                .list_feedback_requests(ListFeedbackRequestsRequest {
                    session_id: session_id.clone(),
                })
                .await
                .unwrap();
            if let Some(request) = requests
                .into_iter()
                .find(|r| r.status == FeedbackStatus::Pending)
            {
                return request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("feedback published")
}
fn reply(request: &FeedbackRequestSnapshot) -> AnswerFeedbackRequest {
    AnswerFeedbackRequest {
        session_id: request.session_id.clone(),
        request_id: request.request_id.clone(),
        answers: vec![FeedbackAnswer {
            selected_options: vec![0],
            text: "保留源文件".into(),
        }],
    }
}

#[tokio::test]
async fn wait_answer_is_tool_result_and_duplicate_reply_is_idempotent() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&question("wait"))),
            ModelScript::Events(message_events(&assistant_text("done", "已记录"))),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
    )
    .await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run = runtime
        .submit_input(input(&session, "生成文档"))
        .await
        .unwrap()
        .run;
    let request = pending(&runtime, &session).await;
    assert_eq!(model.take_requests().len(), 1);
    let answer = reply(&request);
    let first = runtime.answer_feedback(answer.clone()).await.unwrap();
    assert_eq!(
        runtime.answer_feedback(answer.clone()).await.unwrap(),
        first
    );
    let mut wrong = answer;
    wrong.answers[0].text = "另一个答案".into();
    assert!(runtime.answer_feedback(wrong).await.is_err());
    assert_eq!(
        wait_for_terminal(&runtime, &session, &run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    let conversation = &requests[0].conversation.messages;
    assert_eq!(
        conversation
            .iter()
            .filter(|m| matches!(m, ConversationMessage::User(_)))
            .count(),
        1
    );
    let result = conversation
        .iter()
        .find_map(|m| {
            if let ConversationMessage::Tool(m) = m {
                m.result.content.as_single_json()
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(result["answers"][0]["selected_options"], json!(["PDF"]));
    assert_eq!(result["answers"][0]["text"], "保留源文件");
    assert_eq!(
        store.load_feedback(&session).await.unwrap()[0].status,
        FeedbackStatus::Answered
    );
}

#[tokio::test]
async fn end_turn_does_not_call_model_again_until_new_user_reply() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&question("end_turn"))),
            ModelScript::Events(message_events(&assistant_text("after-feedback", "已修改"))),
        ],
    ));
    let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
    let runtime = runtime_with_store(
        model.clone(),
        store.clone(),
        RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
    )
    .await;
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let first = runtime
        .submit_input(input(&session, "审阅初稿"))
        .await
        .unwrap()
        .run;
    let request = pending(&runtime, &session).await;
    assert_eq!(
        wait_for_terminal(&runtime, &session, &first.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    assert_eq!(model.take_requests().len(), 1);
    assert!(
        runtime
            .submit_input(input(&session, "未明确取消的新指令"))
            .await
            .is_err()
    );
    runtime.answer_feedback(reply(&request)).await.unwrap();
    let runs = runtime
        .list_runs(assistant_protocol::ListRunsRequest {
            session_id: session.clone(),
        })
        .await
        .unwrap()
        .runs;
    assert_eq!(runs.len(), 2);
    let second = runs.iter().find(|r| r.run_id != first.run_id).unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &session, &second.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Completed
    );
    let requests = model.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .conversation
            .messages
            .iter()
            .filter(|m| matches!(m, ConversationMessage::User(_)))
            .count(),
        2
    );
    let result = requests[0]
        .conversation
        .messages
        .iter()
        .find_map(|m| {
            if let ConversationMessage::Tool(m) = m {
                m.result.content.as_single_json()
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(result["status"], "waiting_for_user");
    assert!(result.get("answers").is_none());
}

#[tokio::test]
async fn cancellation_rejects_late_answers_and_stops_waiting_run() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [ModelScript::Events(message_events(&question("wait")))],
    ));
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run = runtime
        .submit_input(input(&session, "提问"))
        .await
        .unwrap()
        .run;
    let request = pending(&runtime, &session).await;
    let cancel = CancelFeedbackRequest {
        session_id: session.clone(),
        request_id: request.request_id.clone(),
    };
    let first = runtime.cancel_feedback(cancel.clone()).await.unwrap();
    assert_eq!(runtime.cancel_feedback(cancel).await.unwrap(), first);
    assert!(runtime.answer_feedback(reply(&request)).await.is_err());
    assert_eq!(
        wait_for_terminal(&runtime, &session, &run.run_id)
            .await
            .status,
        assistant_protocol::RunStatus::Cancelled
    );
    assert_eq!(model.take_requests().len(), 1);
}

#[tokio::test]
async fn schema_defaults_and_invalid_arguments_never_publish_partial_requests() {
    let invalid = [
        json!({"questions":[]}),
        json!({"questions":[{"question":" "}]}),
        json!({"questions":[{"question":"x"}],"mode":"async"}),
        json!({"questions":[{"question":"x","multiple":true}]}),
        json!({"questions":[{"question":"x","options":[{"label":"one"}]}]}),
        json!({"questions":[{"question":"x","options":[{"label":"one"},{"label":" one "}]}]}),
        json!({"questions":[{"question":"x","header":"12345678901234567"}]}),
        json!({"questions":[{"question":"x","id":"invented"}]}),
        json!({"questions":vec![json!({"question":"x"});5]}),
    ];
    for arguments in invalid {
        let mut call = question("wait");
        if let AssistantPart::ToolCall(c) = &mut call.parts[0] {
            c.arguments = arguments.clone();
        }
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [
                ModelScript::Events(message_events(&call)),
                ModelScript::Events(message_events(&assistant_text("done", "recovered"))),
            ],
        ));
        let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        let run = runtime
            .submit_input(input(&session, "test"))
            .await
            .unwrap()
            .run;
        assert_eq!(
            wait_for_terminal(&runtime, &session, &run.run_id)
                .await
                .status,
            assistant_protocol::RunStatus::Completed,
            "{arguments}"
        );
        assert!(
            runtime
                .list_feedback_requests(ListFeedbackRequestsRequest {
                    session_id: session
                })
                .await
                .unwrap()
                .is_empty(),
            "{arguments}"
        );
        let requests = model.take_requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].conversation.messages.iter().any(|m| matches!(m,ConversationMessage::Tool(t) if t.result.status == agent_types::ToolResultStatus::Error)));
    }
    let mut call = question("wait");
    if let AssistantPart::ToolCall(c) = &mut call.parts[0] {
        c.arguments = json!({"questions":[{"question":"你的想法？"}]});
    }
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&call)),
            ModelScript::Events(message_events(&assistant_text("done", "ok"))),
        ],
    ));
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run = runtime
        .submit_input(input(&session, "text"))
        .await
        .unwrap()
        .run;
    let request = pending(&runtime, &session).await;
    assert_eq!(request.mode, assistant_protocol::FeedbackMode::Wait);
    assert!(request.questions[0].options.is_empty());
    assert!(!request.questions[0].multiple);
    let mut answer = reply(&request);
    answer.answers[0].selected_options.clear();
    runtime.answer_feedback(answer).await.unwrap();
    wait_for_terminal(&runtime, &session, &run.run_id).await;
}

#[tokio::test]
async fn mixed_and_duplicate_batches_reject_all_calls_before_side_effects() {
    for malformed in [false, true] {
        let side_effect = ScriptedTool::succeed("effect", json!("done"), OrderLog::default());
        let mut registry = ToolRegistry::new();
        registry.register(side_effect.clone()).unwrap();
        let mut call = question("end_turn");
        if malformed && let AssistantPart::ToolCall(c) = &mut call.parts[0] {
            c.arguments = json!({"questions":null});
        }
        call.parts.insert(
            0,
            AssistantPart::ToolCall(ToolCall {
                id: ToolCallId::new("effect-call").unwrap(),
                name: ToolName::new("effect").unwrap(),
                arguments: json!({}),
            }),
        );
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [
                ModelScript::Events(message_events(&call)),
                ModelScript::Events(message_events(&assistant_text("done", "retry alone"))),
            ],
        ));
        let runtime = runtime_with_tools(model, registry.snapshot());
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        let run = runtime
            .submit_input(input(&session, "mixed"))
            .await
            .unwrap()
            .run;
        wait_for_terminal(&runtime, &session, &run.run_id).await;
        assert!(side_effect.executed_inputs().is_empty());
        assert!(
            runtime
                .list_feedback_requests(ListFeedbackRequestsRequest {
                    session_id: session
                })
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn feedback_failures_leave_retryable_state_without_fake_user_messages() {
    for (mode, stage) in [("wait", 2), ("end_turn", 3)] {
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [
                ModelScript::Events(message_events(&question(mode))),
                ModelScript::Events(message_events(&assistant_text("done", "ok"))),
            ],
        ));
        let store = Arc::new(super::store::FaultInjectingStore::healthy());
        let runtime = runtime_with_store(
            model,
            store.clone(),
            RuntimeConfig::new(NonZeroUsize::new(32).unwrap()),
        )
        .await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        let run = runtime
            .submit_input(input(&session, "failure"))
            .await
            .unwrap()
            .run;
        let request = pending(&runtime, &session).await;
        if mode == "end_turn" {
            wait_for_terminal(&runtime, &session, &run.run_id).await;
        }
        store.feedback_failure.store(stage, Ordering::SeqCst);
        assert!(runtime.answer_feedback(reply(&request)).await.is_err());
        assert_eq!(
            store.load_feedback(&session).await.unwrap()[0].status,
            FeedbackStatus::Pending
        );
        assert_eq!(
            runtime
                .list_runs(assistant_protocol::ListRunsRequest {
                    session_id: session.clone()
                })
                .await
                .unwrap()
                .runs
                .len(),
            1
        );
        runtime.answer_feedback(reply(&request)).await.unwrap();
        let runs = runtime
            .list_runs(assistant_protocol::ListRunsRequest {
                session_id: session.clone(),
            })
            .await
            .unwrap()
            .runs;
        for run in runs {
            wait_for_terminal(&runtime, &session, &run.run_id).await;
        }
    }
}

#[tokio::test]
async fn end_turn_survives_restart_without_auto_execution_and_wait_is_invalidated() {
    for mode in ["wait", "end_turn"] {
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [
                ModelScript::Events(message_events(&question(mode))),
                ModelScript::Events(message_events(&assistant_text("done", "ok"))),
            ],
        ));
        let store = Arc::new(crate::storage::VolatileRuntimeStore::default());
        let config = RuntimeConfig::new(NonZeroUsize::new(32).unwrap());
        let runtime = runtime_with_store(model.clone(), store.clone(), config).await;
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        let run = runtime
            .submit_input(input(&session, "restart"))
            .await
            .unwrap()
            .run;
        let request = pending(&runtime, &session).await;
        if mode == "end_turn" {
            wait_for_terminal(&runtime, &session, &run.run_id).await;
        }
        runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .unwrap();
        let recovered = runtime_with_store(model.clone(), store.clone(), config).await;
        let records = recovered
            .list_feedback_requests(ListFeedbackRequestsRequest {
                session_id: session.clone(),
            })
            .await
            .unwrap();
        assert_eq!(
            records[0].status,
            if mode == "wait" {
                FeedbackStatus::Invalidated
            } else {
                FeedbackStatus::Pending
            }
        );
        assert_eq!(model.take_requests().len(), 1);
        if mode == "wait" {
            assert!(recovered.answer_feedback(reply(&request)).await.is_err());
        } else {
            recovered.answer_feedback(reply(&request)).await.unwrap();
            let runs = recovered
                .list_runs(assistant_protocol::ListRunsRequest {
                    session_id: session.clone(),
                })
                .await
                .unwrap()
                .runs;
            for run in runs {
                wait_for_terminal(&recovered, &session, &run.run_id).await;
            }
        }
    }
}

#[tokio::test]
async fn goal_end_turn_pauses_and_wait_cancel_does_not_leave_running_goal() {
    for mode in ["wait", "end_turn"] {
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [ModelScript::Events(message_events(&question(mode)))],
        ));
        let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
        let session = runtime
            .create_session(CreateSessionRequest::default())
            .await
            .unwrap()
            .session
            .session_id;
        let mut goal_input = input(&session, "finish goal");
        goal_input.mode = SubmitInputMode::StartGoal;
        let run = runtime.submit_input(goal_input).await.unwrap().run;
        let request = pending(&runtime, &session).await;
        if mode == "wait" {
            runtime
                .cancel_feedback(CancelFeedbackRequest {
                    session_id: session.clone(),
                    request_id: request.request_id,
                })
                .await
                .unwrap();
        }
        wait_for_terminal(&runtime, &session, &run.run_id).await;
        let controller = runtime.session_for_test(&session).await;
        assert!(matches!(
            controller
                .lock_state()
                .unwrap()
                .goal
                .as_ref()
                .unwrap()
                .state,
            crate::goal::GoalState::Paused(_)
        ));
        assert_eq!(model.take_requests().len(), 1);
    }
}

#[path = "feedback_queue.rs"]
mod queue;
