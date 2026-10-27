use super::*;

struct GatedQuestionModel {
    inner: Arc<ScriptedModelService>,
    first: AtomicBool,
    entered: Notify,
    released: Notify,
}
impl ModelService for GatedQuestionModel {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }
    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }
    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                self.released.notified().await;
            }
            self.inner.stream(request, context).await
        })
    }
}

#[tokio::test]
async fn end_turn_blocks_preexisting_queue_and_reply_precedes_it() {
    let inner = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&question("end_turn"))),
            ModelScript::Events(message_events(&assistant_text(
                "feedback-answer",
                "处理回答",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "queued-answer",
                "处理旧队列",
            ))),
        ],
    ));
    let model = Arc::new(GatedQuestionModel {
        inner: inner.clone(),
        first: AtomicBool::new(true),
        entered: Notify::new(),
        released: Notify::new(),
    });
    let runtime = runtime_with_tools(model.clone(), ToolSetSnapshot::default());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let first = runtime
        .submit_started_input(input(&session, "initial"))
        .await
        .unwrap();
    model.entered.notified().await;
    let queued = runtime
        .submit_input(input(&session, "old queued"))
        .await
        .unwrap();
    model.released.notify_one();
    let request = pending(&runtime, &session).await;
    wait_for_terminal(&runtime, &session, &first.run.run_id).await;
    assert!(queued.run.is_none());
    assert_eq!(inner.take_requests().len(), 1);
    runtime.answer_feedback(reply(&request)).await.unwrap();
    wait_for_terminal(
        &runtime,
        &session,
        &runtime
            .wait_for_input_claim(&session, &queued.input_id)
            .await
            .unwrap()
            .run_id,
    )
    .await;
    let requests = inner.take_requests();
    assert_eq!(requests.len(), 2);
    let user_text = |request: &ModelRequest| {
        request
            .conversation
            .messages
            .iter()
            .filter_map(|m| {
                if let ConversationMessage::User(u) = m {
                    Some(serde_json::to_string(u).unwrap())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    let reply_users = user_text(&requests[0]);
    assert_eq!(reply_users.len(), 2);
    assert!(reply_users[1].contains("回复助手的问题"));
    assert!(!reply_users.iter().any(|s| s.contains("old queued")));
    assert!(
        user_text(&requests[1])
            .last()
            .unwrap()
            .contains("old queued")
    );
}

#[tokio::test]
async fn concurrent_answer_and_cancel_choose_one_terminal_fact() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&question("wait"))),
            ModelScript::Events(message_events(&assistant_text("done", "ok"))),
        ],
    ));
    let runtime = runtime_with_tools(model, ToolSetSnapshot::default());
    let session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let run = runtime
        .submit_started_input(input(&session, "race"))
        .await
        .unwrap()
        .run;
    let request = pending(&runtime, &session).await;
    let (answer, cancel) = tokio::join!(
        runtime.answer_feedback(reply(&request)),
        runtime.cancel_feedback(CancelFeedbackRequest {
            session_id: session.clone(),
            request_id: request.request_id.clone()
        })
    );
    assert_ne!(answer.is_ok(), cancel.is_ok());
    wait_for_terminal(&runtime, &session, &run.run_id).await;
    let records = runtime
        .list_feedback_requests(ListFeedbackRequestsRequest {
            session_id: session,
        })
        .await
        .unwrap();
    assert_eq!(
        records[0].status,
        if answer.is_ok() {
            FeedbackStatus::Answered
        } else {
            FeedbackStatus::Cancelled
        }
    );
}

#[tokio::test]
async fn failed_publication_and_duplicate_feedback_calls_do_not_open_waits() {
    for duplicate in [false, true] {
        let mut call = question("wait");
        if duplicate {
            let mut extra = call.parts[0].clone();
            if let AssistantPart::ToolCall(c) = &mut extra {
                c.id = ToolCallId::new("another-question").unwrap();
            }
            call.parts.push(extra);
        }
        let model = Arc::new(ScriptedModelService::new(
            model_capabilities(true),
            8192,
            [
                ModelScript::Events(message_events(&call)),
                ModelScript::Events(message_events(&assistant_text("done", "recovered"))),
            ],
        ));
        let store = Arc::new(super::super::store::FaultInjectingStore::healthy());
        if !duplicate {
            store.feedback_failure.store(1, Ordering::SeqCst);
        }
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
            .submit_started_input(input(&session, "publish failure"))
            .await
            .unwrap()
            .run;
        wait_for_terminal(&runtime, &session, &run.run_id).await;
        assert!(store.load_feedback(&session).await.unwrap().is_empty());
    }
}
