//! 只操作本场景新建 Home，覆盖问答终态与排队输入的跨介质故障恢复。
use super::*;
use assistant_protocol::{
    FeedbackAnswer, FeedbackMode, FeedbackOption, FeedbackQuestion, FeedbackRequestSnapshot,
    FeedbackStatus,
};
use assistant_runtime::FeedbackMutation;

fn fixture(
    mode: FeedbackMode,
) -> (
    TempDir,
    StorageEngine,
    NewStoredInput,
    FeedbackRequestSnapshot,
) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&root).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f02-questions-")
        .tempdir_in(root)
        .unwrap();
    let mut engine = open_engine(&home);
    let session = session_id("question-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    let input = NewStoredInput {
        feedback_answer: None,
        agent_shell_target: None,
        input_id: InputId::new("question-input").unwrap(),
        session_id: session.clone(),
        idempotency_key: None,
        agent_variant: assistant_protocol::AgentVariant::Build,
        origin: InputOrigin::User,
        goal_binding: None,
        cross_session: None,
        channel_source: Some(desktop_channel_source()),
        skill_activation: None,
        mcp_selection: None,
        message: raw_user_message("question-user", "ask a question"),
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: 1000,
    };
    engine.accept_input(input.clone()).unwrap();
    claim_fixture(&mut engine, &input);
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "question-start".into(),
            session_id: session.clone(),
            input_id: input.input_id.clone(),
            run_id: fixture_run_id(&input).clone(),
            message: Some(input.message.clone()),
            shell: None,
            reasoning_effort: None,
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            created_at_ms: 1500,
        })
        .unwrap();
    let question = FeedbackRequestSnapshot {
        request_id: "question-request".into(),
        session_id: session.clone(),
        run_id: fixture_run_id(&input).clone(),
        tool_call_id: assistant_protocol::ToolCallId::new("call-1").unwrap(),
        mode,
        questions: vec![FeedbackQuestion {
            question: "choose".into(),
            header: None,
            options: vec![FeedbackOption {
                label: "A".into(),
                description: None,
            }],
            multiple: false,
        }],
        status: FeedbackStatus::Pending,
        answers: None,
    };
    let mut pending = pending_tool_exchange(
        session.as_str(),
        fixture_run_id(&input).as_str(),
        "question-exchange",
    );
    for part in &mut pending.assistant.parts {
        if let AssistantPart::ToolCall(call) = part {
            call.name = ToolName::new("ask_user").unwrap();
        }
    }
    engine.begin_tool_exchange(pending).unwrap();
    start_tool(
        &mut engine,
        session.as_str(),
        fixture_run_id(&input).as_str(),
        "question-exchange",
    );
    engine
        .mutate_feedback(FeedbackMutation {
            request: question.clone(),
            expected_status: None,
        })
        .unwrap();
    let mut results = tool_results();
    if mode == FeedbackMode::Wait {
        // 等待工具已因取消返回错误，Run 结算应在同一可靠帧收敛仍 pending 的 UI 控制。
        results[0].result.status = ToolResultStatus::Error;
        results[0].result.content = ToolResultContent::text("feedback cancelled");
    } else {
        results[0].result.content =
            ToolResultContent::json(serde_json::json!({"status":"waiting_for_user"}));
    }
    engine
        .complete_tool_exchange(CompletedToolExchange {
            operation_id: "question-closed".into(),
            receipt: ExchangeReceipt::new("question-exchange").unwrap(),
            session_id: session,
            run_id: fixture_run_id(&input).clone(),
            step: 1,
            results,
            activation_message: None,
            skill_activations: Vec::new(),
            completed_at_ms: 2000,
        })
        .unwrap();
    (home, engine, input, question)
}

fn settle(engine: &mut StorageEngine, input: &NewStoredInput, status: RunStatus) {
    engine
        .settle_run(StoredRunSettlement {
            operation_id: "question-parent-end".into(),
            session_id: input.session_id.clone(),
            run_id: fixture_run_id(input).clone(),
            status,
            cancel_requested: status == RunStatus::Cancelled,
            error: None,
            queued_user_message: None,
            messages: Vec::new(),
            message_step: None,
            goal_effect: None,
            proxy_report: None,
            finished_at_ms: 2200,
        })
        .unwrap();
}

fn count(engine: &StorageEngine, table: &str) -> i64 {
    engine
        .connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn cancelled_question_is_file_only_and_original_zero_message_receipt_recovers_after_parent_control_release()
 {
    let (home, mut engine, input, question) = fixture(FeedbackMode::EndTurn);
    settle(&mut engine, &input, RunStatus::Completed);
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id=?1",
                [fixture_run_id(&input).as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        count(&engine, "session_feedback"),
        1,
        "pending outlives its terminal parent"
    );
    let mut cancelled = question.clone();
    cancelled.status = FeedbackStatus::Cancelled;
    engine.connection.execute_batch("CREATE TRIGGER pause_question_finalization BEFORE DELETE ON body_appends BEGIN SELECT RAISE(ABORT,'injected question finalization'); END;").unwrap();
    assert!(
        engine
            .mutate_feedback(FeedbackMutation {
                request: cancelled.clone(),
                expected_status: Some(FeedbackStatus::Pending)
            })
            .is_err()
    );
    assert_eq!(count(&engine, "session_feedback"), 1);
    assert_eq!(count(&engine, "body_appends"), 1);
    assert!(engine.load_feedback(&input.session_id).is_err());
    let path = body_path(&engine.session_directory(&input.session_id).unwrap(), 1);
    let written = fs::read(&path).unwrap();
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_question_finalization")
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    engine.load_runtime().unwrap();
    assert_eq!(
        engine.load_feedback(&input.session_id).unwrap(),
        vec![cancelled.clone()]
    );
    assert_eq!(count(&engine, "session_feedback"), 0);
    assert_eq!(count(&engine, "body_appends"), 0);
    assert_eq!(fs::read(&path).unwrap(), written);
    assert_eq!(
        engine
            .load_conversation(&input.session_id)
            .unwrap()
            .messages
            .len(),
        3
    );
    engine
        .mutate_feedback(FeedbackMutation {
            request: cancelled,
            expected_status: Some(FeedbackStatus::Pending),
        })
        .unwrap();
    assert!(
        engine
            .mutate_feedback(FeedbackMutation {
                request: question,
                expected_status: None
            })
            .is_err(),
        "history must not reopen a control"
    );
    assert_eq!(fs::read(path).unwrap(), written);
}

#[test]
fn end_turn_answer_and_queued_input_recover_atomically_without_publishing_the_queued_message() {
    let (home, mut engine, input, question) = fixture(FeedbackMode::EndTurn);
    settle(&mut engine, &input, RunStatus::Completed);
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE run_id=?1",
                [fixture_run_id(&input).as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let mut waiting = input.clone();
    waiting.input_id = InputId::new("waiting-input").unwrap();
    waiting.message = raw_user_message("waiting-user", "waiting input");
    waiting.accepted_at_ms = 2400;
    engine.accept_input(waiting.clone()).unwrap();
    let before: (String, i64) = engine
        .connection
        .query_row(
            "SELECT queued_message_json,priority_order FROM inputs WHERE input_id=?1",
            [waiting.input_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut answer = input.clone();
    answer.input_id = InputId::new("answer-input").unwrap();

    answer.idempotency_key = Some(IdempotencyKey::new("feedback:question-request").unwrap());
    answer.message = raw_user_message("answer-user", "response awaiting execution");
    answer.accepted_at_ms = 2600;
    let mut answered = question.clone();
    answered.status = FeedbackStatus::Answered;
    answered.answers = Some(vec![FeedbackAnswer {
        selected_options: vec![0],
        text: "feedback answer".into(),
    }]);
    answer.feedback_answer = Some(answered.clone());
    let path = body_path(&engine.session_directory(&input.session_id).unwrap(), 1);
    let original = fs::read(&path).unwrap();
    let mut invalid = answer.clone();
    invalid.feedback_answer.as_mut().unwrap().questions[0].question = "changed question".into();
    assert!(engine.accept_input(invalid).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(count(&engine, "body_appends"), 0);
    engine.connection.execute_batch("CREATE TRIGGER pause_answer_finalization BEFORE DELETE ON body_appends BEGIN SELECT RAISE(ABORT,'injected answer finalization'); END;").unwrap();
    assert!(engine.accept_input(answer.clone()).is_err());
    assert_eq!(count(&engine, "session_feedback"), 1);
    assert_eq!(count(&engine, "body_appends"), 1);
    assert_eq!(
        count(&engine, "runs"),
        0,
        "unrelated waiting inputs have no Run"
    );
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE input_id=?1",
                [answer.input_id.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let unchanged: (String, i64) = engine
        .connection
        .query_row(
            "SELECT queued_message_json,priority_order FROM inputs WHERE input_id=?1",
            [waiting.input_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(unchanged, before);
    assert!(engine.load_feedback(&input.session_id).is_err());
    assert!(engine.load_conversation(&input.session_id).is_err());
    let written = fs::read(&path).unwrap();
    engine
        .connection
        .execute_batch("DROP TRIGGER pause_answer_finalization")
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    let recovered = engine.load_runtime().unwrap();
    assert!(
        recovered
            .runs
            .iter()
            .all(|run| run.status == RunStatus::Accepted)
    );
    assert_eq!(count(&engine, "runs"), 0);
    assert_eq!(count(&engine, "session_feedback"), 0);
    assert_eq!(count(&engine, "body_appends"), 0);
    assert_eq!(
        engine.load_feedback(&input.session_id).unwrap(),
        vec![answered]
    );
    let queued = engine
        .load_input_for_id(&input.session_id, &answer.input_id)
        .unwrap()
        .unwrap();
    assert_eq!(queued.state, assistant_runtime::StoredInputState::Queued);
    assert_eq!(queued.queued_message, Some(answer.message.clone()));
    let waiting_after: (String, i64) = engine
        .connection
        .query_row(
            "SELECT queued_message_json,priority_order FROM inputs WHERE input_id=?1",
            [waiting.input_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(waiting_after, (before.0, before.1 + 1));
    assert_eq!(fs::read(&path).unwrap(), written);
    assert!(engine.accept_input(answer.clone()).unwrap().is_duplicate());
    assert_eq!(fs::read(&path).unwrap(), written);
    let facts = engine
        .load_conversation_history(ConversationOwner::MainSession {
            session_id: input.session_id.clone(),
        })
        .unwrap()
        .facts;
    assert_eq!(facts.runs.len(), 1);
    assert_eq!(facts.inputs.len(), 1, "unconsumed inputs are not history");
    assert_eq!(
        engine
            .load_conversation(&input.session_id)
            .unwrap()
            .messages
            .len(),
        3
    );
    claim_fixture(&mut engine, &answer);
    engine
        .commit_user_message(UserMessageCommit {
            operation_id: "answer-start".into(),
            session_id: input.session_id.clone(),
            input_id: answer.input_id.clone(),
            run_id: fixture_run_id(&answer),
            message: Some(answer.message),
            shell: None,
            reasoning_effort: None,
            context_management_mode: agent_model::ContextManagementMode::LocalSummary,
            created_at_ms: 3000,
        })
        .unwrap();
    assert_eq!(
        engine
            .load_conversation(&input.session_id)
            .unwrap()
            .messages
            .len(),
        4
    );
}

#[test]
fn run_settlement_freezes_invalidated_question_and_only_releases_matching_pending_control() {
    for (mode, status) in [
        (FeedbackMode::Wait, RunStatus::Completed),
        (FeedbackMode::EndTurn, RunStatus::Cancelled),
    ] {
        let (_home, mut engine, input, mut question) = fixture(mode);
        settle(&mut engine, &input, status);
        question.status = FeedbackStatus::Invalidated;
        assert_eq!(count(&engine, "session_feedback"), 0);
        assert_eq!(
            engine.load_feedback(&input.session_id).unwrap(),
            vec![question]
        );
        assert_eq!(
            engine
                .load_conversation(&input.session_id)
                .unwrap()
                .messages
                .len(),
            3
        );
    }
}
