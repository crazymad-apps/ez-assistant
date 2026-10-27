//! Fork 的可靠前缀与执行历史边界；只使用脚本模型和无副作用的未知工具结果。

use super::*;
use crate::StoredHistoryFacts;
use assistant_protocol::{ConversationOwner, ForkSessionRequest, RetryRunRequest, RunStatus};

struct SecondStepGate {
    inner: Arc<ScriptedModelService>,
    calls: AtomicUsize,
    entered: Notify,
    released: Notify,
}

impl ModelService for SecondStepGate {
    fn capabilities(&self) -> &ModelCapabilities {
        self.inner.capabilities()
    }

    fn context_window_tokens(&self) -> u64 {
        self.inner.context_window_tokens()
    }

    fn stream(&self, request: ModelRequest, context: ModelCallContext) -> ModelStreamFuture<'_> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                self.entered.notify_one();
                self.released.notified().await;
            }
            self.inner.stream(request, context).await
        })
    }
}

fn input(session_id: &SessionId) -> SubmitInputRequest {
    SubmitInputRequest {
        session_id: session_id.clone(),
        message: "fork history fixture".to_owned(),
        mode: assistant_protocol::SubmitInputMode::Normal,
        variant: assistant_protocol::AgentVariant::Build,
        attachment_ids: Vec::new(),
        quotes: Vec::new(),
        skill_name: None,
        mcp_server_key: None,
        idempotency_key: None,
    }
}

async fn history(runtime: &AssistantRuntime, session_id: &SessionId) -> StoredHistoryFacts {
    runtime
        .store
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: session_id.clone(),
        })
        .await
        .unwrap()
        .facts
}

async fn fork_at(runtime: &AssistantRuntime, source: &SessionId, point: &str) -> SessionId {
    let generation = runtime
        .store
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: source.clone(),
        })
        .await
        .unwrap()
        .generation;
    runtime
        .fork_session(ForkSessionRequest {
            session_id: source.clone(),
            fork_point: assistant_protocol::MessageId::new(point).unwrap(),
            expected_generation: generation,
        })
        .await
        .unwrap()
        .session
        .session_id
}

#[tokio::test]
async fn running_and_completed_forks_at_a_tool_step_keep_only_the_reliable_prefix() {
    let inner = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&assistant_tool_call(
                "first-tool-step",
                "missing_fixture_tool",
            ))),
            ModelScript::Events(message_events(&assistant_text("future-final", "done"))),
        ],
    ));
    let model = Arc::new(SecondStepGate {
        inner: inner.clone(),
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        released: Notify::new(),
    });
    let runtime = runtime(model.clone());
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let submitted = runtime.submit_started_input(input(&source)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), model.entered.notified())
        .await
        .expect("second model call starts after reliable tool closure");
    let running_fork = fork_at(&runtime, &source, "first-tool-step").await;
    let frozen = history(&runtime, &running_fork).await;
    assert_eq!(frozen.runs.len(), 1);
    let run = &frozen.runs[0];
    assert!(run.inherited && run.partial);
    assert!(run.status.is_none() && run.finished_at_ms.is_none() && run.error.is_none());
    assert!(run.started_at_ms.is_some());
    let prefix = runtime.conversation_snapshot(&running_fork).await.unwrap();
    assert_eq!(
        prefix.messages.len(),
        3,
        "User, Assistant and its Tool closure"
    );
    prefix.validate_tool_exchange_pairs().unwrap();
    let ConversationMessage::Tool(tool) = &prefix.messages[2] else {
        panic!("Fork retains the tool result after its selected Assistant");
    };
    assert_eq!(run.last_message_id.as_ref(), Some(&tool.id));
    assert!(frozen.message_origins[1].step.is_some());
    assert_eq!(
        frozen.message_origins[1].step,
        frozen.message_origins[2].step
    );
    let view = runtime
        .get_session_view(GetSessionViewRequest {
            session_id: running_fork.clone(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert!(view.active_run.is_none() && view.runs.is_empty());
    assert!(
        matches!(&view.conversation.items[0], assistant_protocol::ConversationItem::User(user) if user.inherited)
    );
    let assistant_protocol::ConversationItem::Assistant(projected) = &view.conversation.items[1]
    else {
        panic!("inherited Assistant projection");
    };
    assert!(projected.inherited && projected.partial);
    assert_eq!(projected.run_id.as_ref(), Some(&submitted.run.run_id));
    assert_eq!(projected.step, frozen.message_origins[1].step);
    assert!(projected.status.is_none() && projected.finished_at_ms.is_none());
    let detail = runtime
        .get_tool_detail(assistant_protocol::GetToolDetailRequest {
            owner: ConversationOwner::MainSession {
                session_id: running_fork.clone(),
            },
            message_id: assistant_protocol::MessageId::new("first-tool-step").unwrap(),
            call_id: assistant_protocol::ToolCallId::new("call-1").unwrap(),
        })
        .await
        .unwrap()
        .snapshot
        .value;
    assert_eq!(detail.run_id.as_ref(), Some(&submitted.run.run_id));
    assert_eq!(
        detail.owner,
        ConversationOwner::MainSession {
            session_id: running_fork.clone()
        }
    );
    model.released.notify_one();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &submitted.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    assert_eq!(history(&runtime, &running_fork).await, frozen);
    let completed_fork = fork_at(&runtime, &source, "first-tool-step").await;
    assert_eq!(history(&runtime, &completed_fork).await, frozen);
    let repeated = fork_at(&runtime, &running_fork, "first-tool-step").await;
    assert_eq!(history(&runtime, &repeated).await, frozen);
    assert_eq!(
        inner.take_requests().len(),
        2,
        "Fork does not execute retained tools or models"
    );
}

#[tokio::test]
async fn fork_preserves_failed_and_retried_attempts_without_duplicating_the_input() {
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(false),
        8192,
        [
            ModelScript::FailEstablishment(ModelError::Provider {
                message: "deterministic non-retryable provider fixture".to_owned(),
                status: Some(400),
            }),
            ModelScript::Events(message_events(&assistant_text("retry-final", "recovered"))),
        ],
    ));
    let runtime = runtime(model.clone());
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let submitted = runtime.submit_started_input(input(&source)).await.unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &submitted.run.run_id)
            .await
            .status,
        RunStatus::Failed
    );
    let retried = runtime
        .retry_run(RetryRunRequest {
            session_id: source.clone(),
            run_id: submitted.run.run_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &retried.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    let target = fork_at(&runtime, &source, "retry-final").await;
    let inherited = history(&runtime, &target).await;
    assert_eq!(inherited.inputs.len(), 1);
    assert_eq!(inherited.message_origins.len(), 2);
    assert_eq!(inherited.runs.len(), 2);
    assert!(inherited.runs.iter().all(|run| {
        run.inherited && !run.partial && run.input_id.as_ref() == Some(&submitted.input_id)
    }));
    let first = inherited
        .runs
        .iter()
        .find(|run| run.run_id == submitted.run.run_id)
        .unwrap();
    let second = inherited
        .runs
        .iter()
        .find(|run| run.run_id == retried.run.run_id)
        .unwrap();
    assert_eq!((first.attempt, first.status), (1, Some(RunStatus::Failed)));
    assert_eq!(
        (second.attempt, second.status),
        (2, Some(RunStatus::Completed))
    );
    assert!(first.finished_at_ms.is_some() && second.finished_at_ms.is_some());
    assert_eq!(model.take_requests().len(), 2);
}

#[tokio::test]
async fn fork_keeps_earlier_empty_retries_but_excludes_empty_retries_after_the_cut() {
    let failure = || {
        ModelScript::FailEstablishment(ModelError::Provider {
            message: "empty retry fixture".into(),
            status: Some(400),
        })
    };
    let model = Arc::new(ScriptedModelService::new(
        model_capabilities(true),
        8192,
        [
            ModelScript::Events(message_events(&assistant_tool_call(
                "first-attempt-tool",
                "missing_fixture_tool",
            ))),
            failure(),
            failure(),
            ModelScript::Events(message_events(&assistant_text(
                "other-input-final",
                "other input",
            ))),
            failure(),
            ModelScript::Events(message_events(&assistant_text(
                "fourth-attempt-final",
                "recovered",
            ))),
            ModelScript::Events(message_events(&assistant_text(
                "rewritten-other-input",
                "replacement",
            ))),
        ],
    ));
    let runtime = runtime(model);
    let source = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .unwrap()
        .session
        .session_id;
    let first = runtime.submit_started_input(input(&source)).await.unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &first.run.run_id)
            .await
            .status,
        RunStatus::Failed
    );
    let second = runtime
        .retry_run(RetryRunRequest {
            session_id: source.clone(),
            run_id: first.run.run_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &second.run.run_id)
            .await
            .status,
        RunStatus::Failed
    );
    let other = runtime.submit_started_input(input(&source)).await.unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &other.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    let third = runtime
        .retry_run(RetryRunRequest {
            session_id: source.clone(),
            run_id: second.run.run_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &third.run.run_id)
            .await
            .status,
        RunStatus::Failed
    );
    let branch = fork_at(&runtime, &source, "other-input-final").await;
    let inherited = history(&runtime, &branch).await;
    assert!(
        inherited
            .runs
            .iter()
            .any(|run| run.run_id == second.run.run_id),
        "the empty retry before the cut is known history"
    );
    assert!(
        !inherited
            .runs
            .iter()
            .any(|run| run.run_id == third.run.run_id),
        "a shared old User is not evidence that a later empty attempt belongs before the cut"
    );
    let fourth = runtime
        .retry_run(RetryRunRequest {
            session_id: source.clone(),
            run_id: third.run.run_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wait_for_terminal(&runtime, &source, &fourth.run.run_id)
            .await
            .status,
        RunStatus::Completed
    );
    let branch = fork_at(&runtime, &source, "fourth-attempt-final").await;
    let inherited = history(&runtime, &branch).await;
    assert_eq!(inherited.runs.len(), 5);
    assert_eq!(inherited.inputs.len(), 2);
    let other_user = history(&runtime, &source)
        .await
        .inputs
        .into_iter()
        .find(|input| input.input_id == other.input_id)
        .unwrap()
        .user_message_id;
    let rewritten = runtime
        .reenter_from_user_message(assistant_protocol::ReenterFromUserMessageRequest {
            session_id: source.clone(),
            message_id: assistant_protocol::MessageId::new(other_user.as_str()).unwrap(),
            message: "replace later input".to_owned(),
            attachment_ids: Vec::new(),
            idempotency_key: None,
            variant: assistant_protocol::AgentVariant::Build,
        })
        .await
        .unwrap();
    wait_for_terminal(&runtime, &source, &rewritten.run.run_id).await;
    let rewritten = history(&runtime, &source).await;
    assert!(
        rewritten
            .runs
            .iter()
            .any(|run| run.run_id == second.run.run_id)
    );
    assert!(
        !rewritten
            .runs
            .iter()
            .any(|run| run.run_id == third.run.run_id || run.run_id == fourth.run.run_id)
    );
}
