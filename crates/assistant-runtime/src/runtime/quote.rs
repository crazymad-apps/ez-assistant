//! Conversation 文本引用的提交校验、来源降级与规范消息写入。

use std::collections::BTreeSet;

use agent_types::{
    PartId as AgentPartId, QuotedTextPart, QuotedTextSourceOwner, QuotedTextSourceRole, UserPart,
};
use assistant_protocol::{
    ConversationOwner, QuotedTextSnapshot, QuotedTextSourceRoleSnapshot, SessionId,
};

use super::AssistantRuntime;
use crate::{ConversationMessageLocationRequest, RuntimeError, RuntimeResult};

const MAX_QUOTES: usize = 16;
const MAX_QUOTE_EXACT_BYTES: usize = 8 * 1024;
const MAX_QUOTE_CONTEXT_CHARS: usize = 128;
const MAX_QUOTE_LABEL_BYTES: usize = 1024;
const MAX_QUOTE_VISIBLE_BYTES: usize = 32 * 1024;

pub(super) fn validate_quotes(quotes: &[QuotedTextSnapshot]) -> RuntimeResult<()> {
    if quotes.len() > MAX_QUOTES {
        return Err(RuntimeError::InvalidRequest {
            reason: "too many quoted text parts",
        });
    }
    let mut ids = BTreeSet::new();
    let mut total = 0_usize;
    for quote in quotes {
        total = total
            .checked_add(quote.exact.len())
            .ok_or(RuntimeError::InvalidRequest {
                reason: "quoted text is too large",
            })?;
        let range_len = quote
            .text_end_utf16
            .checked_sub(quote.text_start_utf16)
            .ok_or(RuntimeError::InvalidRequest {
                reason: "quoted text range is invalid",
            })?;
        let exact_utf16 = u32::try_from(quote.exact.encode_utf16().count()).map_err(|_| {
            RuntimeError::InvalidRequest {
                reason: "quoted text range is invalid",
            }
        })?;
        if quote.exact.trim().is_empty()
            || quote.exact.len() > MAX_QUOTE_EXACT_BYTES
            || quote.prefix.chars().count() > MAX_QUOTE_CONTEXT_CHARS
            || quote.suffix.chars().count() > MAX_QUOTE_CONTEXT_CHARS
            || quote.source_label.trim().is_empty()
            || quote.source_label.len() > MAX_QUOTE_LABEL_BYTES
            || range_len == 0
            || range_len != exact_utf16
            || !ids.insert(quote.quote_id.as_str())
            || total > MAX_QUOTE_VISIBLE_BYTES
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "quoted text is invalid",
            });
        }
    }
    Ok(())
}

impl AssistantRuntime {
    /// 核对 direct locator；来源 stale 或跨 Session 时只降级定位能力，不修改冻结正文。
    pub(super) async fn normalize_quotes(
        &self,
        target_session_id: &SessionId,
        quotes: &[QuotedTextSnapshot],
    ) -> RuntimeResult<Vec<QuotedTextSnapshot>> {
        validate_quotes(quotes)?;
        let mut normalized = Vec::with_capacity(quotes.len());
        for quote in quotes {
            let mut quote = quote.clone();
            quote.source_available = false;
            if owner_session_id(&quote.source_owner) == target_session_id {
                let message_id =
                    agent_types::MessageId::new(quote.source_message_id.as_str().to_owned())
                        .map_err(|_| RuntimeError::InvalidRequest {
                            reason: "quote source message id is invalid",
                        })?;
                let location = self
                    .store
                    .locate_conversation_message(ConversationMessageLocationRequest {
                        owner: quote.source_owner.clone(),
                        message_id,
                    })
                    .await
                    .map_err(|source| RuntimeError::from_store("locate quote source", source))?;
                quote.source_available = location.is_some_and(|location| {
                    location.generation == quote.source_generation
                        && location.display_ordinal.is_some()
                });
            }
            normalized.push(quote);
        }
        Ok(normalized)
    }
}

/// 新 Session 尚不可能拥有既有来源；保留内容并统一关闭 locator。
pub(super) fn deactivate_quote_sources(
    quotes: &[QuotedTextSnapshot],
) -> RuntimeResult<Vec<QuotedTextSnapshot>> {
    validate_quotes(quotes)?;
    Ok(quotes
        .iter()
        .cloned()
        .map(|mut quote| {
            quote.source_available = false;
            quote
        })
        .collect())
}

pub(super) fn insert_quotes(
    message: &mut agent_types::UserMessage,
    quotes: &[QuotedTextSnapshot],
) -> RuntimeResult<()> {
    let insertion = message
        .parts
        .iter()
        .position(|part| {
            matches!(
                part,
                UserPart::FileReferences(_) | UserPart::InternalContext(_)
            )
        })
        .unwrap_or(message.parts.len());
    let quoted = quotes
        .iter()
        .map(|quote| {
            Ok(UserPart::QuotedText(QuotedTextPart {
                quote_id: AgentPartId::new(quote.quote_id.as_str().to_owned()).map_err(|_| {
                    RuntimeError::InvalidRequest {
                        reason: "quote id is invalid",
                    }
                })?,
                exact: quote.exact.clone(),
                prefix: quote.prefix.clone(),
                suffix: quote.suffix.clone(),
                source_owner: match &quote.source_owner {
                    ConversationOwner::MainSession { session_id } => {
                        QuotedTextSourceOwner::MainSession {
                            session_id: session_id.as_str().to_owned(),
                        }
                    }
                    ConversationOwner::ChildTask {
                        session_id,
                        child_task_id,
                    } => QuotedTextSourceOwner::ChildTask {
                        session_id: session_id.as_str().to_owned(),
                        child_task_id: child_task_id.as_str().to_owned(),
                    },
                },
                source_generation: quote.source_generation,
                source_message_id: agent_types::MessageId::new(
                    quote.source_message_id.as_str().to_owned(),
                )
                .map_err(|_| RuntimeError::InvalidRequest {
                    reason: "quote source message id is invalid",
                })?,
                text_start_utf16: quote.text_start_utf16,
                text_end_utf16: quote.text_end_utf16,
                source_role: match quote.source_role {
                    QuotedTextSourceRoleSnapshot::User => QuotedTextSourceRole::User,
                    QuotedTextSourceRoleSnapshot::Assistant => QuotedTextSourceRole::Assistant,
                },
                source_label: quote.source_label.clone(),
                source_created_at_ms: quote.source_created_at_ms,
                source_available: quote.source_available,
            }))
        })
        .collect::<RuntimeResult<Vec<_>>>()?;
    message.parts.splice(insertion..insertion, quoted);
    Ok(())
}

fn owner_session_id(owner: &ConversationOwner) -> &SessionId {
    match owner {
        ConversationOwner::MainSession { session_id }
        | ConversationOwner::ChildTask { session_id, .. } => session_id,
    }
}
