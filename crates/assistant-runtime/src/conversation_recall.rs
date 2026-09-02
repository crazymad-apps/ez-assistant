//! Runtime 绑定的 Conversation Recall 与不透明续读引用。

use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc, time::Duration};

use agent_memory::{
    MemoryPropertyValue, MemoryRecall, MemoryRecallError, MemoryRecallFuture, MemoryRecallRequest,
    MemoryRecallResponse, RecallItem, RecallOrigin, RecallReadDirection, RecallReadWindow,
    RecallReferenceReadFuture, RecallReferenceReadRequest, RecallReferenceReader, RecallScope,
    RecallSourceId,
};
use agent_types::{AssistantPart, ConversationMessage, MessageId, UserPart};
use assistant_protocol::{ConversationOwner, SessionId, WorkspaceId};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio_util::sync::CancellationToken;

use crate::{
    ConversationMessageLocationRequest, ConversationRawWindowRequest, ConversationSearchRequest,
    ConversationSearchScope, RuntimeStore, StoreErrorKind,
};

const REFERENCE_VERSION: u8 = 1;
const CONVERSATION_SOURCE: &str = "conversation";
const MAX_RAW_SCAN: usize = 256;
const MAX_ITEM_CHARS: usize = 4_000;
const MAX_TOTAL_UTF8_BYTES: usize = 24 * 1024;
const RECALL_TIMEOUT: Duration = Duration::from_secs(5);

/// HMAC 保护的 Recall 引用编码器；密钥只由 Runtime Host 持有。
pub struct HmacRecallReferenceCodec {
    key: [u8; 32],
}

impl HmacRecallReferenceCodec {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    fn encode(&self, payload: &RecallReferencePayload) -> Result<String, MemoryRecallError> {
        let payload =
            serde_json::to_vec(payload).map_err(|_| MemoryRecallError::ReferenceInvalid)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key)
            .map_err(|_| MemoryRecallError::ReferenceInvalid)?;
        mac.update(&payload);
        let signature = mac.finalize().into_bytes();
        Ok(format!(
            "v1.{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    fn decode(&self, reference: &str) -> Result<RecallReferencePayload, MemoryRecallError> {
        let mut segments = reference.split('.');
        if segments.next() != Some("v1") {
            return Err(MemoryRecallError::ReferenceInvalid);
        }
        let payload = segments
            .next()
            .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
            .ok_or(MemoryRecallError::ReferenceInvalid)?;
        let signature = segments
            .next()
            .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
            .ok_or(MemoryRecallError::ReferenceInvalid)?;
        if segments.next().is_some() {
            return Err(MemoryRecallError::ReferenceInvalid);
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key)
            .map_err(|_| MemoryRecallError::ReferenceInvalid)?;
        mac.update(&payload);
        mac.verify_slice(&signature)
            .map_err(|_| MemoryRecallError::ReferenceInvalid)?;
        serde_json::from_slice(&payload).map_err(|_| MemoryRecallError::ReferenceInvalid)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecallReferencePayload {
    version: u8,
    source: String,
    caller_session_id: SessionId,
    scope: RecallScope,
    workspace_id: Option<WorkspaceId>,
    owner: ConversationOwner,
    generation: u64,
    message_id: MessageId,
    message_ordinal: u64,
}

pub(crate) struct RuntimeConversationRecall {
    store: Arc<dyn RuntimeStore>,
    codec: Arc<HmacRecallReferenceCodec>,
    caller_session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
}

/// 通过签名与调用方范围校验后，供 Runtime 产品投影使用的 Recall 来源位置。
pub(crate) struct ResolvedRecallReference {
    pub(crate) owner: ConversationOwner,
    pub(crate) message_id: MessageId,
}

impl RuntimeConversationRecall {
    pub(crate) fn new(
        store: Arc<dyn RuntimeStore>,
        codec: Arc<HmacRecallReferenceCodec>,
        caller_session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
    ) -> Self {
        Self {
            store,
            codec,
            caller_session_id,
            workspace_id,
        }
    }

    /// 校验不透明引用并只返回 UI 导航所需的最小位置。
    ///
    /// 该入口刻意不暴露 generation、ordinal 或签名载荷，避免桌面层依赖 Recall
    /// 引用的内部编码。
    pub(crate) fn resolve_reference(
        &self,
        reference: &str,
    ) -> Result<ResolvedRecallReference, MemoryRecallError> {
        let payload = self.codec.decode(reference)?;
        self.validate_reference(&payload)?;
        Ok(ResolvedRecallReference {
            owner: payload.owner,
            message_id: payload.message_id,
        })
    }

    async fn search(
        &self,
        query: String,
        scope: RecallScope,
        limit: NonZeroUsize,
        sources: Option<Vec<RecallSourceId>>,
    ) -> Result<MemoryRecallResponse, MemoryRecallError> {
        if let Some(sources) = sources
            && sources
                .iter()
                .any(|source| source.as_str() != CONVERSATION_SOURCE)
        {
            return Err(MemoryRecallError::invalid_input(
                "conversation recall supports only the conversation source",
            ));
        }
        let store_scope = match scope {
            RecallScope::Session => ConversationSearchScope::Session {
                session_id: self.caller_session_id.clone(),
            },
            RecallScope::Workspace => ConversationSearchScope::Workspace {
                workspace_id: self
                    .workspace_id
                    .clone()
                    .ok_or(MemoryRecallError::ScopeUnavailable)?,
            },
            RecallScope::Global => ConversationSearchScope::Global,
        };
        let page = self
            .store
            .search_conversations(ConversationSearchRequest {
                query,
                scope: store_scope,
                limit: limit.get(),
            })
            .await
            .map_err(map_search_error)?;
        let source_id = RecallSourceId::new(CONVERSATION_SOURCE)
            .expect("static conversation source id is valid");
        let mut items = Vec::with_capacity(page.hits.len());
        for hit in page.hits {
            let reference = self.codec.encode(&RecallReferencePayload {
                version: REFERENCE_VERSION,
                source: CONVERSATION_SOURCE.to_owned(),
                caller_session_id: self.caller_session_id.clone(),
                scope,
                workspace_id: self.workspace_id.clone(),
                owner: hit.owner.clone(),
                generation: hit.generation,
                message_id: hit.message_id,
                message_ordinal: hit.message_ordinal,
            })?;
            let mut attributes = owner_attributes(&hit.owner);
            attributes.insert(
                "created_at_ms".to_owned(),
                MemoryPropertyValue::Number(hit.created_at_ms.into()),
            );
            items.push(RecallItem {
                content: hit.text,
                origins: vec![RecallOrigin {
                    source_id: source_id.clone(),
                    reference: Some(reference),
                }],
                attributes,
            });
        }
        Ok(MemoryRecallResponse {
            items,
            failures: Vec::new(),
            truncated: page.partial,
            window: None,
        })
    }

    async fn read(
        &self,
        reference: String,
        direction: RecallReadDirection,
        limit: NonZeroUsize,
    ) -> Result<MemoryRecallResponse, MemoryRecallError> {
        let payload = self.codec.decode(&reference)?;
        self.validate_reference(&payload)?;
        let location = self
            .store
            .locate_conversation_message(ConversationMessageLocationRequest {
                owner: payload.owner.clone(),
                message_id: payload.message_id.clone(),
            })
            .await
            .map_err(map_read_error)?
            .ok_or(MemoryRecallError::ReferenceStale)?;
        if location.generation != payload.generation
            || location.message_ordinal != payload.message_ordinal
        {
            return Err(MemoryRecallError::ReferenceStale);
        }
        let ordinal = usize::try_from(location.message_ordinal)
            .map_err(|_| MemoryRecallError::ReferenceStale)?;

        let scan = limit
            .get()
            .saturating_mul(8)
            .clamp(limit.get(), MAX_RAW_SCAN);
        let start = match direction {
            RecallReadDirection::Before => ordinal.saturating_sub(scan),
            RecallReadDirection::After => ordinal.saturating_add(1),
            RecallReadDirection::Around => ordinal.saturating_sub(scan / 2),
        };
        let window = self
            .store
            .load_conversation_raw_window(ConversationRawWindowRequest {
                owner: payload.owner.clone(),
                generation: location.generation,
                start,
                limit: scan,
            })
            .await
            .map_err(map_read_error)?;
        let mut visible = window
            .conversation
            .messages
            .iter()
            .enumerate()
            .filter_map(|(offset, message)| {
                searchable_message(message)
                    .map(|(id, role, content)| (window.start + offset, id.clone(), role, content))
            })
            .collect::<Vec<_>>();
        select_visible_window(&mut visible, ordinal, direction, limit.get());
        let source_id = RecallSourceId::new(CONVERSATION_SOURCE)
            .expect("static conversation source id is valid");
        let mut items = Vec::with_capacity(visible.len());
        for (raw_ordinal, id, role, content) in visible {
            let message_ordinal =
                u64::try_from(raw_ordinal).map_err(|_| MemoryRecallError::ReferenceStale)?;
            let reference = self.codec.encode(&RecallReferencePayload {
                version: REFERENCE_VERSION,
                source: CONVERSATION_SOURCE.to_owned(),
                caller_session_id: self.caller_session_id.clone(),
                scope: payload.scope,
                workspace_id: payload.workspace_id.clone(),
                owner: payload.owner.clone(),
                generation: location.generation,
                message_id: id,
                message_ordinal,
            })?;
            let mut attributes = owner_attributes(&payload.owner);
            attributes.insert(
                "role".to_owned(),
                MemoryPropertyValue::String(role.to_owned()),
            );
            items.push(RecallItem {
                content,
                origins: vec![RecallOrigin {
                    source_id: source_id.clone(),
                    reference: Some(reference),
                }],
                attributes,
            });
        }
        let (has_more_before, has_more_after) = match direction {
            RecallReadDirection::Before => (window.start > 0, true),
            RecallReadDirection::After => (true, window.end < window.total),
            RecallReadDirection::Around => (window.start > 0, window.end < window.total),
        };
        Ok(MemoryRecallResponse {
            items,
            failures: Vec::new(),
            truncated: has_more_before || has_more_after,
            window: Some(RecallReadWindow {
                has_more_before,
                has_more_after,
            }),
        })
    }

    fn validate_reference(
        &self,
        payload: &RecallReferencePayload,
    ) -> Result<(), MemoryRecallError> {
        if payload.version != REFERENCE_VERSION
            || payload.source != CONVERSATION_SOURCE
            || payload.caller_session_id != self.caller_session_id
        {
            return Err(MemoryRecallError::ReferenceInvalid);
        }
        match payload.scope {
            RecallScope::Session if owner_session_id(&payload.owner) != &self.caller_session_id => {
                Err(MemoryRecallError::ReferenceInvalid)
            }
            RecallScope::Workspace
                if payload.workspace_id.is_none() || payload.workspace_id != self.workspace_id =>
            {
                Err(MemoryRecallError::ReferenceInvalid)
            }
            _ => Ok(()),
        }
    }
}

impl MemoryRecall for RuntimeConversationRecall {
    fn recall(
        &self,
        request: MemoryRecallRequest,
        cancellation: CancellationToken,
    ) -> MemoryRecallFuture<'_, MemoryRecallResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(MemoryRecallError::Cancelled);
            }
            let mut response = tokio::time::timeout(
                RECALL_TIMEOUT,
                self.search(request.query, request.scope, request.limit, request.sources),
            )
            .await
            .map_err(|_| MemoryRecallError::SourceUnavailable)??;
            apply_output_budget(&mut response);
            Ok(response)
        })
    }
}

impl RecallReferenceReader for RuntimeConversationRecall {
    fn read_reference(
        &self,
        request: RecallReferenceReadRequest,
        cancellation: CancellationToken,
    ) -> RecallReferenceReadFuture<'_, MemoryRecallResponse> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(MemoryRecallError::Cancelled);
            }
            // 搜索与续读共享同一资源预算，但能力接口保持分离；只有具备稳定顺序和引用语义的
            // Runtime Conversation Store 才装配续读能力。
            let mut response = tokio::time::timeout(
                RECALL_TIMEOUT,
                self.read(request.reference, request.direction, request.limit),
            )
            .await
            .map_err(|_| MemoryRecallError::SourceUnavailable)??;
            apply_output_budget(&mut response);
            Ok(response)
        })
    }
}

fn apply_output_budget(response: &mut MemoryRecallResponse) {
    let mut retained = Vec::with_capacity(response.items.len());
    let mut used_bytes = 0_usize;
    let original_count = response.items.len();
    for mut item in response.items.drain(..) {
        if item.content.chars().count() > MAX_ITEM_CHARS {
            item.content = item.content.chars().take(MAX_ITEM_CHARS).collect();
            response.truncated = true;
        }
        let item_bytes = item.content.len();
        if used_bytes.saturating_add(item_bytes) > MAX_TOTAL_UTF8_BYTES {
            response.truncated = true;
            break;
        }
        used_bytes = used_bytes.saturating_add(item_bytes);
        retained.push(item);
    }
    if retained.len() < original_count {
        response.truncated = true;
    }
    response.items = retained;
}

fn searchable_message(message: &ConversationMessage) -> Option<(&MessageId, &'static str, String)> {
    match message {
        ConversationMessage::User(message) if message.transcript_visibility.is_visible() => {
            let mut parts = Vec::new();
            for part in &message.parts {
                match part {
                    UserPart::Text(part) => parts.push(part.text.clone()),
                    UserPart::FileReferences(references) => parts.extend(
                        references
                            .files
                            .iter()
                            .map(|file| file.original_name.clone()),
                    ),
                    UserPart::QuotedText(quoted) => parts.push(quoted.exact.clone()),
                    UserPart::Injected(_) | UserPart::InternalContext(_) => {}
                }
            }
            Some((&message.id, "user", parts.join("\n")))
        }
        ConversationMessage::Assistant(message) => Some((
            &message.id,
            "assistant",
            message
                .parts
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(part) => Some(part.text.as_str()),
                    AssistantPart::Reasoning(_)
                    | AssistantPart::ToolCall(_)
                    | AssistantPart::ProviderState(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        ConversationMessage::User(_)
        | ConversationMessage::System(_)
        | ConversationMessage::ContextSummary(_)
        | ConversationMessage::Tool(_) => None,
    }
}

fn select_visible_window<T>(
    visible: &mut Vec<(usize, MessageId, &'static str, T)>,
    target_ordinal: usize,
    direction: RecallReadDirection,
    limit: usize,
) {
    match direction {
        RecallReadDirection::Before => {
            visible.retain(|(ordinal, ..)| *ordinal < target_ordinal);
            if visible.len() > limit {
                visible.drain(..visible.len() - limit);
            }
        }
        RecallReadDirection::After => {
            visible.retain(|(ordinal, ..)| *ordinal > target_ordinal);
            visible.truncate(limit);
        }
        RecallReadDirection::Around => {
            let target_index = visible
                .iter()
                .position(|(ordinal, ..)| *ordinal == target_ordinal);
            let start = target_index
                .map(|index| index.saturating_sub(limit / 2))
                .unwrap_or(0)
                .min(visible.len().saturating_sub(limit));
            let end = start.saturating_add(limit).min(visible.len());
            visible.drain(end..);
            visible.drain(..start);
        }
    }
}

fn owner_session_id(owner: &ConversationOwner) -> &SessionId {
    match owner {
        ConversationOwner::MainSession { session_id }
        | ConversationOwner::ChildTask { session_id, .. } => session_id,
    }
}

fn owner_attributes(owner: &ConversationOwner) -> BTreeMap<String, MemoryPropertyValue> {
    let mut attributes = BTreeMap::new();
    match owner {
        ConversationOwner::MainSession { .. } => {
            attributes.insert(
                "conversation_kind".to_owned(),
                MemoryPropertyValue::String("session".to_owned()),
            );
        }
        ConversationOwner::ChildTask { .. } => {
            attributes.insert(
                "conversation_kind".to_owned(),
                MemoryPropertyValue::String("child_task".to_owned()),
            );
        }
    }
    attributes
}

fn map_search_error(error: crate::StoreError) -> MemoryRecallError {
    match error.kind() {
        StoreErrorKind::InvalidInput => MemoryRecallError::invalid_input(error.message()),
        StoreErrorKind::Unavailable
        | StoreErrorKind::InvalidData
        | StoreErrorKind::Conflict
        | StoreErrorKind::ResourceUnavailable
        | StoreErrorKind::Internal => MemoryRecallError::SourceUnavailable,
    }
}

fn map_read_error(error: crate::StoreError) -> MemoryRecallError {
    match error.kind() {
        StoreErrorKind::Conflict => MemoryRecallError::ReferenceStale,
        StoreErrorKind::Unavailable
        | StoreErrorKind::InvalidData
        | StoreErrorKind::ResourceUnavailable
        | StoreErrorKind::Internal => MemoryRecallError::SourceUnavailable,
        StoreErrorKind::InvalidInput => MemoryRecallError::ReferenceInvalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::VolatileRuntimeStore;

    fn session_id(value: &str) -> SessionId {
        SessionId::new(value).expect("valid Session ID")
    }

    fn workspace_id(value: &str) -> WorkspaceId {
        WorkspaceId::new(value).expect("valid Workspace ID")
    }

    fn message_id(value: &str) -> MessageId {
        MessageId::new(value).expect("valid message ID")
    }

    fn payload(caller: &SessionId, scope: RecallScope) -> RecallReferencePayload {
        RecallReferencePayload {
            version: REFERENCE_VERSION,
            source: CONVERSATION_SOURCE.to_owned(),
            caller_session_id: caller.clone(),
            scope,
            workspace_id: Some(workspace_id("workspace-a")),
            owner: ConversationOwner::MainSession {
                session_id: caller.clone(),
            },
            generation: 7,
            message_id: message_id("message-7"),
            message_ordinal: 11,
        }
    }

    fn recall(caller: &SessionId, workspace: Option<WorkspaceId>) -> RuntimeConversationRecall {
        RuntimeConversationRecall::new(
            Arc::new(VolatileRuntimeStore::default()),
            Arc::new(HmacRecallReferenceCodec::new([23; 32])),
            caller.clone(),
            workspace,
        )
    }

    #[test]
    fn signed_reference_round_trips_and_rejects_tampering() {
        let caller = session_id("session-a");
        let codec = HmacRecallReferenceCodec::new([19; 32]);
        let expected = payload(&caller, RecallScope::Session);
        let encoded = codec.encode(&expected).expect("encode reference");

        assert_eq!(
            codec.decode(&encoded).expect("decode reference").message_id,
            expected.message_id
        );

        let mut tampered = encoded.into_bytes();
        let last = tampered.last_mut().expect("non-empty reference");
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("ASCII reference");
        assert!(matches!(
            codec.decode(&tampered),
            Err(MemoryRecallError::ReferenceInvalid)
        ));
    }

    #[test]
    fn reference_validation_binds_caller_scope_and_workspace() {
        let caller = session_id("session-a");
        let runtime = recall(&caller, Some(workspace_id("workspace-a")));
        assert!(
            runtime
                .validate_reference(&payload(&caller, RecallScope::Session))
                .is_ok()
        );

        let mut wrong_caller = payload(&caller, RecallScope::Session);
        wrong_caller.caller_session_id = session_id("session-b");
        assert_eq!(
            runtime.validate_reference(&wrong_caller),
            Err(MemoryRecallError::ReferenceInvalid)
        );

        let mut wrong_owner = payload(&caller, RecallScope::Session);
        wrong_owner.owner = ConversationOwner::MainSession {
            session_id: session_id("session-b"),
        };
        assert_eq!(
            runtime.validate_reference(&wrong_owner),
            Err(MemoryRecallError::ReferenceInvalid)
        );

        let mut wrong_workspace = payload(&caller, RecallScope::Workspace);
        wrong_workspace.workspace_id = Some(workspace_id("workspace-b"));
        assert_eq!(
            runtime.validate_reference(&wrong_workspace),
            Err(MemoryRecallError::ReferenceInvalid)
        );
    }

    #[test]
    fn bounded_windows_keep_the_target_only_for_around_reads() {
        let items = (0..7)
            .map(|ordinal| {
                (
                    ordinal,
                    message_id(&format!("message-{ordinal}")),
                    "user",
                    ordinal,
                )
            })
            .collect::<Vec<_>>();

        let mut before = items.clone();
        select_visible_window(&mut before, 3, RecallReadDirection::Before, 2);
        assert_eq!(before.iter().map(|item| item.0).collect::<Vec<_>>(), [1, 2]);

        let mut after = items.clone();
        select_visible_window(&mut after, 3, RecallReadDirection::After, 2);
        assert_eq!(after.iter().map(|item| item.0).collect::<Vec<_>>(), [4, 5]);

        let mut around = items;
        select_visible_window(&mut around, 3, RecallReadDirection::Around, 3);
        assert_eq!(
            around.iter().map(|item| item.0).collect::<Vec<_>>(),
            [2, 3, 4]
        );
    }

    #[test]
    fn output_budget_truncates_items_without_splitting_utf8() {
        let source_id = RecallSourceId::new(CONVERSATION_SOURCE).expect("source ID");
        let item = |content: String| RecallItem {
            content,
            origins: vec![RecallOrigin {
                source_id: source_id.clone(),
                reference: None,
            }],
            attributes: BTreeMap::new(),
        };
        let mut response = MemoryRecallResponse {
            items: vec![
                item("你".repeat(MAX_ITEM_CHARS + 1)),
                item("a".repeat(MAX_ITEM_CHARS)),
                item("b".repeat(MAX_ITEM_CHARS)),
                item("c".repeat(MAX_ITEM_CHARS)),
                item("d".repeat(MAX_ITEM_CHARS)),
            ],
            failures: Vec::new(),
            truncated: false,
            window: None,
        };

        apply_output_budget(&mut response);

        assert!(response.truncated);
        assert_eq!(response.items.len(), 4);
        assert_eq!(response.items[0].content.chars().count(), MAX_ITEM_CHARS);
        assert!(
            response.items[0]
                .content
                .is_char_boundary(response.items[0].content.len())
        );
    }

    #[test]
    fn hidden_runtime_user_message_is_not_searchable() {
        let hidden = ConversationMessage::User(agent_types::UserMessage {
            id: message_id("runtime-hidden"),
            origin: agent_types::UserMessageOrigin::Runtime,
            transcript_visibility: agent_types::TranscriptVisibility::Hidden,
            parts: vec![agent_types::UserPart::Text(agent_types::TextPart {
                id: agent_types::PartId::new("runtime-hidden-text").expect("part ID"),
                text: "runtime-hidden-recall-token".to_owned(),
            })],
        });
        assert!(searchable_message(&hidden).is_none());

        let visible = ConversationMessage::User(agent_types::UserMessage {
            id: message_id("user-visible"),
            origin: agent_types::UserMessageOrigin::User,
            transcript_visibility: agent_types::TranscriptVisibility::Visible,
            parts: vec![agent_types::UserPart::Text(agent_types::TextPart {
                id: agent_types::PartId::new("user-visible-text").expect("part ID"),
                text: "visible-recall-token".to_owned(),
            })],
        });
        assert_eq!(
            searchable_message(&visible).map(|(_, role, text)| (role, text)),
            Some(("user", "visible-recall-token".to_owned()))
        );
    }
}
