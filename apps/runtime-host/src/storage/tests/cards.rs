//! 新建 .runtime-test SQLite；工具/卡片/快照均为通用夹具，无用户数据。
use super::*;
use assistant_protocol::{ListMessageCardsRequest, MessageCardSnapshot, MessageCardSource};
use assistant_runtime::{MessageCardSnapshotWrite, NewMessageCard};

#[test]
fn message_cards_are_receipt_deduplicated_private_and_survive_restart() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f03-card-store-")
        .tempdir_in(base)
        .unwrap();
    let mut engine = open_engine(&home);
    seed_session_and_run(&mut engine, "card-session", "card-run");
    engine
        .connection
        .execute(
            "UPDATE runs SET status='running',started_at_ms=1500 WHERE run_id='card-run'",
            [],
        )
        .unwrap();
    engine
        .begin_tool_exchange(pending_tool_exchange(
            "card-session",
            "card-run",
            "card-receipt",
        ))
        .unwrap();
    let owner = ConversationOwner::MainSession {
        session_id: session_id("card-session"),
    };
    let card = NewMessageCard {
        card_id: "card-first".into(),
        owner: owner.clone(),
        run_id: Some(run_id("card-run")),
        tool_call_id: assistant_protocol::ToolCallId::new("call-1").unwrap(),
        title: "Card".into(),
        source: MessageCardSource::Plugin {
            name: "ui-fixture".into(),
            entry_id: "counter".into(),
            published_sha256: "a".repeat(64),
        },
        arguments: serde_json::json!({"count":1}),
        tool_result: serde_json::json!({"_meta":{"private":"UI_PRIVATE_RESULT"}}),
        source_fingerprint: None,
    };
    assert!(
        engine.publish_message_card(card.clone()).is_err(),
        "not-started calls cannot publish"
    );
    start_tool(&mut engine, "card-session", "card-run", "card-receipt");
    let first = engine.publish_message_card(card.clone()).unwrap();
    let mut duplicate = card;
    duplicate.card_id = "ignored-generated-id".into();
    assert_eq!(
        engine
            .publish_message_card(duplicate)
            .unwrap()
            .summary
            .card_id,
        first.summary.card_id
    );
    let write = |revision| MessageCardSnapshotWrite {
        owner: owner.clone(),
        card_id: first.summary.card_id.clone(),
        expected_revision: revision,
        snapshot: MessageCardSnapshot {
            schema: "v1".into(),
            value: serde_json::json!({"private":"PRIVATE_SNAPSHOT"}),
        },
    };
    assert!(engine.save_message_card_snapshot(write(0)).unwrap().saved);
    assert!(!engine.save_message_card_snapshot(write(0)).unwrap().saved);
    let body = engine
        .load_conversation(&session_id("card-session"))
        .unwrap();
    assert!(
        !serde_json::to_string(&body)
            .unwrap()
            .contains("UI_PRIVATE_RESULT")
    );
    engine
        .complete_tool_exchange(CompletedToolExchange {
            step: 1,
            operation_id: "card-complete".into(),
            receipt: ExchangeReceipt::new("card-receipt").unwrap(),
            session_id: session_id("card-session"),
            run_id: run_id("card-run"),
            results: tool_results(),
            activation_message: None,
            skill_activations: vec![],
            completed_at_ms: 2300,
        })
        .unwrap();
    drop(engine);
    let mut engine = open_engine(&home);
    let stored = engine
        .get_message_card(owner.clone(), "card-first".into())
        .unwrap()
        .unwrap();
    assert_eq!(stored.summary.snapshot_revision, 1);
    assert_eq!(
        stored.snapshot.unwrap().value["private"],
        "PRIVATE_SNAPSHOT"
    );
    let list = engine
        .list_message_cards(ListMessageCardsRequest {
            owner: owner.clone(),
            run_ids: vec![run_id("card-run")],
            message_ids: vec![],
            after: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(list.cards.len(), 1);
    assert!(!format!("{list:?}").contains("UI_PRIVATE_RESULT"));
    assert!(
        engine
            .get_message_card(
                ConversationOwner::MainSession {
                    session_id: session_id("different-owner")
                },
                "card-first".into()
            )
            .unwrap()
            .is_none()
    );
    engine
        .create_session(new_session("card-copy", &engine.sessions_directory))
        .unwrap();
    let tx = engine.connection.transaction().unwrap();
    super::super::card::copy_for_fork(
        &tx,
        &session_id("card-session"),
        &session_id("card-copy"),
        &[first.summary.anchor_message_id.as_str().to_owned()]
            .into_iter()
            .collect(),
        &[],
    )
    .unwrap();
    tx.commit().unwrap();
    let copied = engine
        .list_message_cards(ListMessageCardsRequest {
            owner: ConversationOwner::MainSession {
                session_id: session_id("card-copy"),
            },
            run_ids: vec![run_id("card-run")],
            message_ids: vec![],
            after: None,
            limit: 1,
        })
        .unwrap();
    assert_eq!(copied.cards.len(), 1);
    assert!(copied.cards[0].inherited);
    assert_ne!(copied.cards[0].card_id, first.summary.card_id);
    assert_eq!(
        engine
            .connection
            .query_row("SELECT COUNT(*) FROM message_cards", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
