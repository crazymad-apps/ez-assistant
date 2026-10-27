//! 完成指令只在 SQL 保留幂等身份；结果及消息只从文件恢复，无真实 MCP／配置副作用。
use super::*;
use assistant_runtime::{NewStoredSessionCommand, StoredSessionCommand};

pub(super) fn request(session: &SessionId, suffix: &str) -> NewStoredSessionCommand {
    NewStoredSessionCommand {
        input_id: InputId::new(format!("command-{suffix}")).unwrap(),
        session_id: session.clone(),
        idempotency_key: Some(IdempotencyKey::new(format!("key-{suffix}")).unwrap()),
        user_message_id: MessageId::new(format!("command-message-{suffix}")).unwrap(),
        agent_variant: assistant_protocol::AgentVariant::Build,
        command: assistant_protocol::SessionCommand::McpRefresh { server: None },
        accepted_at_ms: 2000,
    }
}

pub(super) fn commit(request: &NewStoredSessionCommand) -> assistant_runtime::SessionCommandCommit {
    let mut message = raw_user_message(
        request.user_message_id.as_str(),
        "{RUNTIME_CONTROL_RESULT_V1} refreshed",
    );
    message.origin = UserMessageOrigin::Runtime;
    assistant_runtime::SessionCommandCommit {
        operation_id: format!("finish-{}", request.input_id),
        input_id: request.input_id.clone(),
        session_id: request.session_id.clone(),
        message,
        result: assistant_runtime::StoredSessionCommandResult::Mcp(
            assistant_protocol::McpRefreshControlResultSnapshot {
                outcome: assistant_protocol::McpRefreshOutcome::Success,
                servers: Vec::new(),
            },
        ),
        committed_at_ms: 3000,
    }
}

#[test]
fn command_commit_releases_payload_but_preserves_identity_and_another_queued_command() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let root = tempfile::Builder::new()
        .prefix("f02-command-file-")
        .tempdir_in(base)
        .unwrap();
    let mut engine = open_engine(&root);
    let session = session_id("command-owner");
    engine
        .create_session(new_session(session.as_str(), &engine.sessions_directory))
        .unwrap();
    let original = request(&session, "original");
    let waiting = request(&session, "waiting");
    engine.accept_session_command(original.clone()).unwrap();
    engine.accept_session_command(waiting.clone()).unwrap();
    let waiting_row = |engine: &StorageEngine| {
        engine.connection.query_row(
        "SELECT command_json,priority_order,user_message_id,accepted_at_ms FROM inputs WHERE input_id='command-waiting'",
        [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?,row.get::<_, String>(2)?,row.get::<_, i64>(3)?)),
    ).unwrap()
    };
    let waiting_bytes = waiting_row(&engine);
    let source = body_path(&engine.session_directory(&session).unwrap(), 1);
    let source_bytes = fs::read(&source).unwrap();
    // 同一会话另一排队载荷损坏，也不能被目标命令的精确提交／幂等查询解码。
    engine
        .connection
        .execute(
            "UPDATE inputs SET command_json='{' WHERE input_id='command-waiting'",
            [],
        )
        .unwrap();
    engine.connection.execute_batch("CREATE TRIGGER fail_command_switch BEFORE UPDATE OF body_generation ON sessions BEGIN SELECT RAISE(ABORT,'injected generation switch failure'); END;").unwrap();
    let result = commit(&original);
    assert!(engine.commit_session_command(result.clone()).is_err());
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(engine.session_generation(&session).unwrap(), 1);
    assert_eq!(engine.connection.query_row("SELECT COUNT(*) FROM inputs WHERE input_kind='command' AND state='queued' AND command_json IS NOT NULL AND user_message_id IS NOT NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 2);
    engine
        .connection
        .execute_batch("DROP TRIGGER fail_command_switch")
        .unwrap();
    let completed = engine.commit_session_command(result.clone()).unwrap();
    assert_eq!(engine.connection.query_row("SELECT COUNT(*) FROM inputs WHERE input_id='command-original' AND state='committed' AND command_json IS NULL AND command_result_json IS NULL AND user_message_id IS NULL AND accepted_at_ms IS NULL AND agent_variant IS NULL AND origin IS NULL AND priority_order IS NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    let changes = engine.connection.total_changes();
    assert_eq!(
        engine.commit_session_command(result.clone()).unwrap(),
        completed
    );
    let duplicate = engine.accept_session_command(original.clone()).unwrap();
    assert!(duplicate.is_duplicate);
    assert_eq!(duplicate.command, completed);
    assert_eq!(engine.connection.total_changes(), changes);
    assert_eq!(engine.session_generation(&session).unwrap(), 2);
    let mut changed = original.clone();
    changed.command = assistant_protocol::SessionCommand::McpRefresh {
        server: Some(McpServerKey::new("different").unwrap()),
    };
    assert_eq!(
        engine.accept_session_command(changed).unwrap_err().kind(),
        StoreErrorKind::Conflict
    );
    let mut collision = super::input_selections::selected_input(&session, "key-collision", false);
    collision.idempotency_key = original.idempotency_key.clone();
    assert!(engine.accept_input(collision).is_err());
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM inputs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    engine
        .connection
        .execute(
            "UPDATE inputs SET command_json=?1 WHERE input_id='command-waiting'",
            [&waiting_bytes.0],
        )
        .unwrap();
    assert_eq!(waiting_row(&engine), waiting_bytes);
    drop(engine);
    let mut engine = open_engine(&root);
    let loaded = engine.load_session_state(&session).unwrap().state;
    assert_eq!(loaded.session_commands.len(), 2);
    assert!(loaded.inputs.is_empty());
    assert!(loaded.runs.is_empty());
    assert_eq!(
        engine
            .load_session_command(&session, &original.input_id)
            .unwrap(),
        Some(completed.clone())
    );
    assert_eq!(waiting_row(&engine), waiting_bytes);
    engine.commit_session_command(commit(&waiting)).unwrap();
    let target = session_id("command-fork");
    let inherited = StoredSessionCommand {
        session_id: target.clone(),
        input_id: InputId::new("inherited-command").unwrap(),
        user_message_id: MessageId::new("inherited-command-message").unwrap(),
        idempotency_key: None,
        ..completed
    };
    let mut fork_message = result.message.clone();
    fork_message.id = inherited.user_message_id.clone();
    let conversation =
        ConversationSnapshot::new(vec![ConversationMessage::User(fork_message.clone())]);
    let fork = engine
        .fork_session(SessionFork {
            source_session_id: session.clone(),
            source_generation: 3,
            session: new_session(target.as_str(), &engine.sessions_directory),
            conversation,
            attachments: Vec::new(),
            tool_images: Vec::new(),
            skill_activations: Vec::new(),
            mcp_selections: Vec::new(),
            session_commands: vec![assistant_runtime::ForkedSessionCommand {
                source_input_id: original.input_id.clone(),
                command: inherited.clone(),
            }],
            work_plan: None,
            goal: None,
        })
        .unwrap();
    assert_eq!(fork.session_commands, vec![inherited.clone()]);
    assert_eq!(
        engine
            .connection
            .query_row(
                "SELECT COUNT(*) FROM inputs WHERE session_id='command-fork'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let impact = engine.inspect_session_deletion(&session).unwrap();
    engine
        .delete_session(SessionDeletion {
            session_id: session,
            operation_id: "delete-command-source".into(),
            expected_impact: impact,
        })
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&root);
    let recovered = engine.load_runtime().unwrap();
    assert_eq!(recovered.session_commands, vec![inherited]);
    assert!(recovered.inputs.is_empty());
    assert!(recovered.runs.is_empty());
    assert_eq!(
        engine.load_conversation(&target).unwrap().messages,
        vec![ConversationMessage::User(fork_message)]
    );
}
