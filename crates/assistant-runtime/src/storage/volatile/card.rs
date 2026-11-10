use super::*;
use crate::{MessageCardSnapshotWrite, NewMessageCard, StoredMessageCard, message_card_owner};
use assistant_protocol::{
    ListMessageCardsRequest, ListMessageCardsResult, SaveMessageCardSnapshotResult,
};

impl VolatileRuntimeStore {
    pub(super) fn publish_message_card(
        &self,
        card: NewMessageCard,
    ) -> StoreFuture<'_, StoredMessageCard> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let (session, child) = message_card_owner(&card.owner);
            if !state.sessions.contains_key(session) || child.is_none() != card.run_id.is_some() {
                return Err(conflict("card owner unavailable"));
            }
            let (receipt, anchor) = if let Some(child) = child {
                state
                    .pending_child_tool_exchanges
                    .iter()
                    .find(|(_, e)| {
                        e.session_id == *session
                            && e.child_task_id == *child
                            && e.started_calls.contains(card.tool_call_id.as_str())
                    })
                    .map(|(id, e)| (id.clone(), e.assistant.id.clone()))
            } else {
                state
                    .pending_tool_exchanges
                    .iter()
                    .find(|(_, e)| {
                        e.session_id == *session
                            && Some(&e.run_id) == card.run_id.as_ref()
                            && e.started_calls.contains(card.tool_call_id.as_str())
                    })
                    .map(|(id, e)| (id.clone(), e.assistant.id.clone()))
            }
            .ok_or_else(|| conflict("card requires a started tool call"))?;
            if let Some(existing) = state.message_cards.values().find(|c| {
                c.summary.owner == card.owner
                    && c.receipt_id == receipt
                    && c.tool_call_id == card.tool_call_id
            }) {
                return Ok(existing.clone());
            }
            if state.message_cards.contains_key(&card.card_id) {
                return Err(conflict("card identity exists"));
            }
            state.next_card_position = state
                .next_card_position
                .checked_add(1)
                .ok_or_else(|| conflict("card position exhausted"))?;
            let stored = StoredMessageCard {
                summary: assistant_protocol::MessageCardSummary {
                    card_id: card.card_id.clone(),
                    owner: card.owner,
                    run_id: card.run_id,
                    anchor_message_id: assistant_protocol::MessageId::new(anchor.as_str())
                        .map_err(|_| conflict("card anchor invalid"))?,
                    title: card.title,
                    source: card.source,
                    position: state.next_card_position,
                    snapshot_revision: 0,
                    inherited: false,
                },
                receipt_id: receipt,
                tool_call_id: card.tool_call_id,
                arguments: card.arguments,
                tool_result: card.tool_result,
                source_fingerprint: card.source_fingerprint,
                snapshot: None,
            };
            state.message_cards.insert(card.card_id, stored.clone());
            Ok(stored)
        })
    }

    pub(super) fn list_message_cards(
        &self,
        request: ListMessageCardsRequest,
    ) -> StoreFuture<'_, ListMessageCardsResult> {
        Box::pin(async move {
            if !(1..=100).contains(&request.limit)
                || request.message_ids.len() > 1000
                || request.run_ids.len() > 1000
            {
                return Err(conflict("card query exceeds limit"));
            }
            let state = self.lock()?;
            let child = matches!(request.owner, ConversationOwner::ChildTask { .. });
            let mut cards = state
                .message_cards
                .values()
                .filter(|card| {
                    card.summary.owner == request.owner
                        && card.summary.position > request.after.unwrap_or(0)
                        && (child
                            || request
                                .message_ids
                                .contains(&card.summary.anchor_message_id)
                            || card
                                .summary
                                .run_id
                                .as_ref()
                                .is_some_and(|id| request.run_ids.contains(id)))
                })
                .map(|card| card.summary.clone())
                .collect::<Vec<_>>();
            cards.sort_by_key(|c| c.position);
            let next = if cards.len() > request.limit as usize {
                cards.truncate(request.limit as usize);
                cards.last().map(|c| c.position)
            } else {
                None
            };
            Ok(ListMessageCardsResult { cards, next })
        })
    }

    pub(super) fn get_message_card(
        &self,
        owner: ConversationOwner,
        id: String,
    ) -> StoreFuture<'_, Option<StoredMessageCard>> {
        Box::pin(async move {
            Ok(self
                .lock()?
                .message_cards
                .get(&id)
                .filter(|card| card.summary.owner == owner)
                .cloned())
        })
    }

    pub(super) fn save_message_card_snapshot(
        &self,
        write: MessageCardSnapshotWrite,
    ) -> StoreFuture<'_, SaveMessageCardSnapshotResult> {
        Box::pin(async move {
            let mut state = self.lock()?;
            let card = state
                .message_cards
                .get_mut(&write.card_id)
                .filter(|card| card.summary.owner == write.owner)
                .ok_or_else(|| conflict("card unavailable"))?;
            if card.summary.snapshot_revision != write.expected_revision {
                return Ok(SaveMessageCardSnapshotResult {
                    saved: false,
                    revision: card.summary.snapshot_revision,
                    snapshot: card.snapshot.clone(),
                });
            }
            if card.snapshot.as_ref() != Some(&write.snapshot) {
                card.summary.snapshot_revision = card
                    .summary
                    .snapshot_revision
                    .checked_add(1)
                    .ok_or_else(|| conflict("snapshot revision exhausted"))?;
                card.snapshot = Some(write.snapshot);
            }
            Ok(SaveMessageCardSnapshotResult {
                saved: true,
                revision: card.summary.snapshot_revision,
                snapshot: card.snapshot.clone(),
            })
        })
    }
}
