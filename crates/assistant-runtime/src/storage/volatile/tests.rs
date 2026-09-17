use agent_types::{PartId, TextPart, TranscriptVisibility, UserMessage, UserMessageOrigin};
use assistant_protocol::GoalId;
use sha2::{Digest, Sha256};

use super::*;

fn user_message(
    message_id: &str,
    part_id: &str,
    text: &str,
    origin: UserMessageOrigin,
    visibility: TranscriptVisibility,
) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: MessageId::new(message_id).expect("message ID"),
        origin,
        transcript_visibility: visibility,
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new(part_id).expect("part ID"),
            text: text.to_owned(),
        })],
    })
}

#[test]
fn hidden_runtime_users_do_not_change_display_windows_or_volatile_recall() {
    let visible = user_message(
        "visible-user",
        "visible-user-text",
        "visible-recall-token",
        UserMessageOrigin::User,
        TranscriptVisibility::Visible,
    );
    let hidden = user_message(
        "runtime-hidden-user",
        "runtime-hidden-user-text",
        "runtime-hidden-recall-token",
        UserMessageOrigin::Runtime,
        TranscriptVisibility::Hidden,
    );
    let snapshot = ConversationSnapshot::new(vec![visible, hidden]);
    let session_id = SessionId::new("session-visible-window").expect("session ID");
    let owner = ConversationOwner::MainSession {
        session_id: session_id.clone(),
    };

    let window = conversation_window(
        snapshot.clone(),
        &ConversationWindowRequest {
            owner: owner.clone(),
            generation: 1,
            end: None,
            limit: 1,
        },
    );
    assert_eq!((window.start, window.end, window.total), (0, 1, 1));
    assert_eq!(window.conversation.messages.len(), 2);
    assert!(!window.conversation.messages[1].is_transcript_visible());

    let mut hits = Vec::new();
    collect_volatile_hits(
        &mut hits,
        owner.clone(),
        1,
        1_000,
        &snapshot,
        "runtime-hidden-recall-token",
    );
    assert!(hits.is_empty());
    collect_volatile_hits(
        &mut hits,
        owner,
        1,
        1_000,
        &snapshot,
        "visible-recall-token",
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id.as_str(), "visible-user");
}

#[tokio::test]
async fn volatile_recovery_pauses_a_running_goal_once() {
    let store = VolatileRuntimeStore::default();
    let session_id = SessionId::new("session-goal-recovery").expect("session id");
    let payload = vec![crate::StoredGoalObjectivePart::Text(TextPart {
        id: PartId::new("goal-objective-part").expect("part id"),
        text: "finish safely".to_owned(),
    })];
    let hash = format!(
        "sha256-v1:{:x}",
        Sha256::digest(serde_json::to_vec(&payload).expect("encode objective"))
    );
    store.lock().expect("state").goals.insert(
        session_id.clone(),
        crate::StoredGoal {
            goal_id: GoalId::new("goal-recovery").expect("goal id"),
            session_id,
            objective: crate::StoredGoalObjective {
                source_message_id: MessageId::new("goal-source").expect("message id"),
                payload,
                payload_hash: hash,
            },
            mcp_server_key: None,
            state: StoredGoalState::Running,
            pause_reason: None,
            generation: 1,
            turn: 1,
            budget: crate::StoredGoalBudget {
                max_runs: 20,
                max_total_tokens: 500_000,
                max_consecutive_failures: 3,
                used_runs: 1,
                used_total_tokens: 100,
                usage_complete: true,
            },
            consecutive_failures: 0,
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
        },
    );

    let first = store.load_runtime().await.expect("first recovery");
    assert_eq!(first.goals[0].state, StoredGoalState::Paused);
    assert_eq!(
        first.goals[0].pause_reason,
        Some(StoredGoalPauseReason::RecoveryRequired)
    );
    assert_eq!(first.goals[0].generation, 2);
    let second = store.load_runtime().await.expect("second recovery");
    assert_eq!(second.goals[0].generation, 2);
}

#[tokio::test]
async fn volatile_skill_name_states_are_default_enabled_and_keyed_only_by_name() {
    let store = VolatileRuntimeStore::default();
    assert!(
        store
            .list_skill_name_states()
            .await
            .expect("initial states")
            .is_empty()
    );
    let name = SkillName::parse("review-pr").expect("name");
    store
        .set_skill_enabled(SkillNameStateChange {
            name: name.clone(),
            enabled: false,
            updated_at_ms: 10,
        })
        .await
        .expect("disable");
    store
        .set_skill_enabled(SkillNameStateChange {
            name,
            enabled: true,
            updated_at_ms: 20,
        })
        .await
        .expect("enable");
    assert_eq!(
        store.list_skill_name_states().await.expect("stored states"),
        vec![SkillNameState {
            name: SkillName::parse("review-pr").expect("name"),
            enabled: true,
            updated_at_ms: 20,
        }]
    );
}
