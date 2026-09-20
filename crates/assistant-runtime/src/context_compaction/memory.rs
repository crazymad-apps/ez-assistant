use std::collections::{BTreeMap, BTreeSet};

use agent_memory::{PinnedMemoryCategory, PinnedMemoryEntry, PinnedMemoryId};
use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ToolCallId, ToolResultStatus,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::pinned_memory_limits;

const DELTA_KIND: &str = "pinned_memory_delta";
const DELTA_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryMutationKind {
    Pin,
    Update,
    Unpin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PinnedMemoryOrigin {
    CreatedInSession,
    FrozenSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedMemoryDeltaEntryV1 {
    id: PinnedMemoryId,
    category: PinnedMemoryCategory,
    content: String,
    origin: PinnedMemoryOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedMemoryDeltaV1 {
    kind: String,
    version: u8,
    upserts: Vec<PinnedMemoryDeltaEntryV1>,
    deleted_ids: Vec<PinnedMemoryId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedMemoryToolResultV1 {
    id: PinnedMemoryId,
    category: PinnedMemoryCategory,
    content: String,
}

/// 从完整产品历史重算当前 Session 相对冻结 Pinned Memory 快照的净变化。
pub(super) fn derive_pinned_memory_context(
    history: &ConversationSnapshot,
) -> Result<Option<String>, PinnedMemoryContextError> {
    history
        .validate_tool_exchange_pairs()
        .map_err(|_| PinnedMemoryContextError::InvalidConversation)?;
    validate_latest_programmatic_context(history)?;

    let mut pending = BTreeMap::<ToolCallId, MemoryMutationKind>::new();
    let mut upserts = BTreeMap::<PinnedMemoryId, PinnedMemoryDeltaEntryV1>::new();
    let mut deleted_ids = BTreeSet::<PinnedMemoryId>::new();

    for message in &history.messages {
        match message {
            ConversationMessage::ContextSummary(_) => pending.clear(),
            ConversationMessage::Assistant(assistant) => {
                for part in &assistant.parts {
                    let AssistantPart::ToolCall(call) = part else {
                        continue;
                    };
                    let kind = match call.name.as_str() {
                        "pin_memory" => Some(MemoryMutationKind::Pin),
                        "update_pinned_memory" => Some(MemoryMutationKind::Update),
                        "unpin_memory" => Some(MemoryMutationKind::Unpin),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        pending.insert(call.id.clone(), kind);
                    }
                }
            }
            ConversationMessage::Tool(tool) => {
                let Some(kind) = pending.remove(&tool.result.call_id) else {
                    continue;
                };
                if tool.result.status == ToolResultStatus::Error {
                    continue;
                }
                let value = tool
                    .result
                    .content
                    .as_single_json()
                    .ok_or(PinnedMemoryContextError::MalformedSuccessfulResult)?;
                let result: PinnedMemoryToolResultV1 = serde_json::from_value(value.clone())
                    .map_err(|_| PinnedMemoryContextError::MalformedSuccessfulResult)?;
                apply_mutation(kind, result, &mut upserts, &mut deleted_ids)?;
            }
            ConversationMessage::System(_) | ConversationMessage::User(_) => {}
        }
    }

    if upserts.is_empty() && deleted_ids.is_empty() {
        return Ok(None);
    }
    let delta = PinnedMemoryDeltaV1 {
        kind: DELTA_KIND.to_owned(),
        version: DELTA_VERSION,
        upserts: upserts.into_values().collect(),
        deleted_ids: deleted_ids.into_iter().collect(),
    };
    validate_delta(&delta)?;
    let encoded =
        serde_json::to_string(&delta).map_err(|_| PinnedMemoryContextError::Serialization)?;
    validate_encoded_size(&encoded)?;
    Ok(Some(encoded))
}

fn validate_latest_programmatic_context(
    history: &ConversationSnapshot,
) -> Result<(), PinnedMemoryContextError> {
    let Some(summary) = history
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ConversationMessage::ContextSummary(summary) => Some(summary),
            _ => None,
        })
    else {
        return Ok(());
    };
    let Some(context) = summary.programmatic_context.as_deref() else {
        return Ok(());
    };
    let delta: PinnedMemoryDeltaV1 = serde_json::from_str(context)
        .map_err(|_| PinnedMemoryContextError::InvalidExistingContext)?;
    validate_delta(&delta).map_err(|_| PinnedMemoryContextError::InvalidExistingContext)?;
    validate_encoded_size(context).map_err(|_| PinnedMemoryContextError::InvalidExistingContext)
}

fn apply_mutation(
    kind: MemoryMutationKind,
    result: PinnedMemoryToolResultV1,
    upserts: &mut BTreeMap<PinnedMemoryId, PinnedMemoryDeltaEntryV1>,
    deleted_ids: &mut BTreeSet<PinnedMemoryId>,
) -> Result<(), PinnedMemoryContextError> {
    let entry = PinnedMemoryEntry {
        id: result.id.clone(),
        category: result.category.clone(),
        content: result.content.clone(),
        attributes: BTreeMap::new(),
    };
    entry
        .validate(&pinned_memory_limits())
        .map_err(|_| PinnedMemoryContextError::InvalidEntry)?;

    match kind {
        MemoryMutationKind::Pin => {
            deleted_ids.remove(&result.id);
            upserts.insert(
                result.id.clone(),
                delta_entry(result, PinnedMemoryOrigin::CreatedInSession),
            );
        }
        MemoryMutationKind::Update => {
            let origin = upserts
                .get(&result.id)
                .map_or(PinnedMemoryOrigin::FrozenSnapshot, |entry| {
                    entry.origin.clone()
                });
            deleted_ids.remove(&result.id);
            upserts.insert(result.id.clone(), delta_entry(result, origin));
        }
        MemoryMutationKind::Unpin => {
            let removed = upserts.remove(&result.id);
            if removed
                .as_ref()
                .is_some_and(|entry| entry.origin == PinnedMemoryOrigin::CreatedInSession)
            {
                deleted_ids.remove(&result.id);
            } else {
                deleted_ids.insert(result.id);
            }
        }
    }
    Ok(())
}

fn delta_entry(
    result: PinnedMemoryToolResultV1,
    origin: PinnedMemoryOrigin,
) -> PinnedMemoryDeltaEntryV1 {
    PinnedMemoryDeltaEntryV1 {
        id: result.id,
        category: result.category,
        content: result.content,
        origin,
    }
}

fn validate_delta(delta: &PinnedMemoryDeltaV1) -> Result<(), PinnedMemoryContextError> {
    if delta.kind != DELTA_KIND || delta.version != DELTA_VERSION {
        return Err(PinnedMemoryContextError::UnsupportedSchema);
    }
    let limits = pinned_memory_limits();
    if delta.upserts.len().saturating_add(delta.deleted_ids.len()) > limits.max_entries.get() {
        return Err(PinnedMemoryContextError::TooManyEntries);
    }
    let mut previous_upsert: Option<&PinnedMemoryId> = None;
    let deleted = delta.deleted_ids.iter().collect::<BTreeSet<_>>();
    for upsert in &delta.upserts {
        let entry = PinnedMemoryEntry {
            id: upsert.id.clone(),
            category: upsert.category.clone(),
            content: upsert.content.clone(),
            attributes: BTreeMap::new(),
        };
        entry
            .validate(&limits)
            .map_err(|_| PinnedMemoryContextError::InvalidEntry)?;
        if previous_upsert.is_some_and(|previous| previous >= &upsert.id)
            || deleted.contains(&upsert.id)
        {
            return Err(PinnedMemoryContextError::InvalidOrdering);
        }
        previous_upsert = Some(&upsert.id);
    }
    let mut previous_deleted: Option<&PinnedMemoryId> = None;
    for id in &delta.deleted_ids {
        id.validate(&limits)
            .map_err(|_| PinnedMemoryContextError::InvalidEntry)?;
        if previous_deleted.is_some_and(|previous| previous >= id) {
            return Err(PinnedMemoryContextError::InvalidOrdering);
        }
        previous_deleted = Some(id);
    }
    Ok(())
}

fn validate_encoded_size(encoded: &str) -> Result<(), PinnedMemoryContextError> {
    if encoded.len() > pinned_memory_limits().max_snapshot_bytes.get() {
        return Err(PinnedMemoryContextError::TooLarge);
    }
    Ok(())
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum PinnedMemoryContextError {
    #[error("product conversation is invalid for pinned memory projection")]
    InvalidConversation,
    #[error("successful pinned memory tool result does not match its contract")]
    MalformedSuccessfulResult,
    #[error("pinned memory programmatic context contains an invalid entry")]
    InvalidEntry,
    #[error("existing pinned memory programmatic context is invalid")]
    InvalidExistingContext,
    #[error("pinned memory programmatic context schema is unsupported")]
    UnsupportedSchema,
    #[error("pinned memory programmatic context is not strictly sorted and disjoint")]
    InvalidOrdering,
    #[error("pinned memory programmatic context contains too many effective ids")]
    TooManyEntries,
    #[error("pinned memory programmatic context exceeds its byte limit")]
    TooLarge,
    #[error("pinned memory programmatic context could not be serialized")]
    Serialization,
}

#[cfg(test)]
mod tests {
    use agent_types::{
        AssistantMessage, FinishReason, MessageId, ModelIdentity, ProviderId, ToolCall,
        ToolMessage, ToolName, ToolResult, ToolResultContent,
    };

    use super::*;

    fn id(value: &str) -> MessageId {
        MessageId::new(value).unwrap()
    }

    fn exchange(
        sequence: usize,
        name: &str,
        status: ToolResultStatus,
        value: serde_json::Value,
    ) -> [ConversationMessage; 2] {
        let call_id = ToolCallId::new(format!("call_{sequence}")).unwrap();
        [
            ConversationMessage::Assistant(AssistantMessage {
                id: id(&format!("assistant_{sequence}")),
                model: ModelIdentity::new(ProviderId::new("test").unwrap(), "model"),
                parts: vec![AssistantPart::ToolCall(ToolCall {
                    id: call_id.clone(),
                    name: ToolName::new(name).unwrap(),
                    arguments: serde_json::json!({}),
                })],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            }),
            ConversationMessage::Tool(ToolMessage {
                id: id(&format!("tool_{sequence}")),
                result: ToolResult {
                    call_id,
                    status,
                    content: ToolResultContent::json(value),
                    metadata: None,
                },
            }),
        ]
    }

    fn entry(id: &str, content: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "category": "preference", "content": content})
    }

    fn derive(messages: Vec<ConversationMessage>) -> Option<PinnedMemoryDeltaV1> {
        let encoded =
            derive_pinned_memory_context(&ConversationSnapshot::new(messages)).unwrap()?;
        Some(serde_json::from_str(&encoded).unwrap())
    }

    #[test]
    fn recomputes_sorted_net_changes_from_successful_results() {
        let mut messages = Vec::new();
        messages.extend(exchange(
            1,
            "pin_memory",
            ToolResultStatus::Success,
            entry("b", "one"),
        ));
        messages.extend(exchange(
            2,
            "update_pinned_memory",
            ToolResultStatus::Success,
            entry("b", "two"),
        ));
        messages.extend(exchange(
            3,
            "update_pinned_memory",
            ToolResultStatus::Success,
            entry("a", "old"),
        ));
        messages.extend(exchange(
            4,
            "unpin_memory",
            ToolResultStatus::Success,
            entry("b", "two"),
        ));
        messages.extend(exchange(
            5,
            "unpin_memory",
            ToolResultStatus::Success,
            entry("c", "old"),
        ));

        let delta = derive(messages).unwrap();
        assert_eq!(delta.upserts.len(), 1);
        assert_eq!(delta.upserts[0].id.as_str(), "a");
        assert_eq!(delta.upserts[0].origin, PinnedMemoryOrigin::FrozenSnapshot);
        assert_eq!(
            delta
                .deleted_ids
                .iter()
                .map(PinnedMemoryId::as_str)
                .collect::<Vec<_>>(),
            vec!["c"]
        );
    }

    #[test]
    fn failures_are_ignored_but_malformed_success_is_rejected() {
        let failed = exchange(
            1,
            "pin_memory",
            ToolResultStatus::Error,
            serde_json::json!({"anything": true}),
        );
        assert_eq!(derive(failed.into()), None);

        let malformed = exchange(
            2,
            "pin_memory",
            ToolResultStatus::Success,
            serde_json::json!({"id": "x", "category": "preference"}),
        );
        assert_eq!(
            derive_pinned_memory_context(&ConversationSnapshot::new(malformed.into())),
            Err(PinnedMemoryContextError::MalformedSuccessfulResult)
        );
    }

    #[test]
    fn existing_context_is_strictly_validated_before_recomputation() {
        let history = ConversationSnapshot::new(vec![ConversationMessage::ContextSummary(
            agent_types::ContextSummaryMessage {
                id: id("summary"),
                text: "summary".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
                usage_adjustment: None,
                programmatic_context: Some(
                    r#"{"kind":"pinned_memory_delta","version":2,"upserts":[],"deleted_ids":[]}"#
                        .to_owned(),
                ),
            },
        )]);
        assert_eq!(
            derive_pinned_memory_context(&history),
            Err(PinnedMemoryContextError::InvalidExistingContext)
        );
    }

    #[test]
    fn existing_context_rejects_unknown_fields_and_unsorted_ids() {
        for context in [
            r#"{"kind":"pinned_memory_delta","version":1,"upserts":[],"deleted_ids":[],"extra":true}"#,
            r#"{"kind":"pinned_memory_delta","version":1,"upserts":[],"deleted_ids":["b","a"]}"#,
        ] {
            let history = ConversationSnapshot::new(vec![ConversationMessage::ContextSummary(
                agent_types::ContextSummaryMessage {
                    id: id("summary"),
                    text: "summary".to_owned(),
                    model: None,
                    usage: None,
                    compacted_usage: None,
                    usage_adjustment: None,
                    programmatic_context: Some(context.to_owned()),
                },
            )]);
            assert_eq!(
                derive_pinned_memory_context(&history),
                Err(PinnedMemoryContextError::InvalidExistingContext)
            );
        }
    }

    #[test]
    fn derived_context_enforces_the_shared_snapshot_byte_limit() {
        let content = "x".repeat(pinned_memory_limits().max_content_bytes.get());
        let mut messages = Vec::new();
        for sequence in 0..9 {
            messages.extend(exchange(
                sequence,
                "pin_memory",
                ToolResultStatus::Success,
                entry(&format!("memory-{sequence}"), &content),
            ));
        }
        assert_eq!(
            derive_pinned_memory_context(&ConversationSnapshot::new(messages)),
            Err(PinnedMemoryContextError::TooLarge)
        );
    }

    #[test]
    fn old_summary_without_programmatic_context_is_backfilled_from_raw_history() {
        let mut messages = exchange(
            1,
            "pin_memory",
            ToolResultStatus::Success,
            entry("memory-before-summary", "remembered"),
        )
        .to_vec();
        messages.push(ConversationMessage::ContextSummary(
            agent_types::ContextSummaryMessage {
                id: id("old-summary"),
                text: "old summary".to_owned(),
                model: None,
                usage: None,
                compacted_usage: None,
                usage_adjustment: None,
                programmatic_context: None,
            },
        ));

        let delta = derive(messages).unwrap();
        assert_eq!(delta.upserts[0].id.as_str(), "memory-before-summary");
        assert_eq!(
            delta.upserts[0].origin,
            PinnedMemoryOrigin::CreatedInSession
        );
    }

    #[test]
    fn delta_rejects_more_than_the_shared_effective_id_limit() {
        let delta = PinnedMemoryDeltaV1 {
            kind: DELTA_KIND.to_owned(),
            version: DELTA_VERSION,
            upserts: Vec::new(),
            deleted_ids: (0..=pinned_memory_limits().max_entries.get())
                .map(|index| PinnedMemoryId::new(format!("memory-{index:03}")).unwrap())
                .collect(),
        };
        assert_eq!(
            validate_delta(&delta),
            Err(PinnedMemoryContextError::TooManyEntries)
        );
    }
}
