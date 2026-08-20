//! Desktop 使用的组合快照、分页 Conversation 与安全工具详情投影。

use std::collections::HashMap;

use agent_memory::{MemoryPropertyValue, MemoryRecallResponse};
use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ToolImageReference, ToolResultPart,
    ToolResultStatus, UserPart,
};
use assistant_protocol::{
    ApplicationCapabilities, ApplicationSnapshot, ApprovalQueueSnapshot, AssistantMessageSnapshot,
    AssistantSegment, AttachmentId, ChildTaskTreeItemSnapshot, ChildTaskUsageSnapshot,
    ChildTaskViewSnapshot, ComposerCapabilitiesSnapshot, ConversationFileReference,
    ConversationHistoryHit, ConversationHistoryMatchKind, ConversationHistoryScope,
    ConversationItem, ConversationOwner, ConversationPage, GetApplicationSnapshotRequest,
    GetApplicationSnapshotResult, GetChildTaskViewRequest, GetChildTaskViewResult,
    GetConversationPageAroundMessageRequest, GetConversationPageAroundMessageResult,
    GetConversationPageAroundRunRequest, GetConversationPageAroundRunResult,
    GetConversationRecallWindowRequest, GetConversationRecallWindowResult, GetSessionViewRequest,
    GetSessionViewResult, GetToolDetailRequest, GetToolDetailResult, ImageHandlingMode,
    ListAttachmentsRequest, ListConversationPageRequest, ListConversationPageResult,
    ListSessionsRequest, ListWorkspacesRequest, MessageId, ObservedSnapshot, PartId,
    QueueExecutionState, QueueSnapshot, QueuedInputSnapshot, ReasoningEffortKey,
    ReasoningEffortOptionSnapshot, RecallNavigationTarget, RecallToolDetailFailure,
    RecallToolDetailItem, RecallToolDetailSnapshot, ResourceRefId, RunId, RunSnapshot,
    SearchConversationHistoryRequest, SearchConversationHistoryResult, SessionId,
    SessionListFilter, SessionUsageSnapshot, SessionViewSnapshot, TokenUsageSnapshot,
    ToolActivityStatus, ToolCallId, ToolDetailSnapshot, ToolEventSnapshot, ToolFileReference,
    ToolFileResourceOrigin, ToolFileResourceState, ToolInputSnapshot, UsageTotals,
    UserMessageSnapshot,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use super::AssistantRuntime;
use crate::{
    ConversationMessageLocationRequest, ConversationRawWindowRequest, ConversationSearchRequest,
    ConversationSearchScope, ConversationWindowRequest, RuntimeError, RuntimeResult,
    StoreErrorKind, StoredConversationWindow,
};

const DEFAULT_PAGE_SIZE: usize = 30;
const MAX_PAGE_SIZE: usize = 100;
const SNAPSHOT_ATTEMPTS: usize = 4;
const TOOL_SUMMARY_CHARS: usize = 160;
const TOOL_DETAIL_JSON_CHARS: usize = 64 * 1024;

#[derive(Deserialize, Serialize)]
struct ConversationCursor {
    generation: u64,
    end: usize,
}

struct ProjectionContext {
    run_by_message: HashMap<String, MessageRunProjection>,
    input_by_message: HashMap<String, assistant_protocol::InputId>,
    attachment_by_path: HashMap<String, AttachmentId>,
    feedback_by_message: HashMap<String, assistant_protocol::MessageFeedback>,
    can_fork: bool,
}

struct MessageRunProjection {
    snapshot: RunSnapshot,
    finished_at_ms: Option<i64>,
}

/// Host 通过稳定资源引用解析出的受控本地文件；不会进入应用协议。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedToolFileResource {
    pub path: String,
    pub display_name: String,
    pub media_type: Option<String>,
    pub origin: ToolFileResourceOrigin,
    pub tool_image: Option<ToolImageReference>,
}

impl AssistantRuntime {
    /// 按产品导出规则生成当前主会话的 Markdown。
    ///
    /// 导出只包含用户正文、附件名称、助手正文和工具调用摘要；Reasoning、
    /// Provider 私有状态、隐藏注入和完整工具输出不会进入结果。
    pub async fn export_session_markdown(&self, session_id: &SessionId) -> RuntimeResult<String> {
        let session = self.session(session_id)?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let snapshot = session.conversation_snapshot()?;
        let mut markdown = String::new();
        let title = session.summary()?.title;
        markdown.push_str("# ");
        markdown.push_str(&title);
        markdown.push_str("\n\n");
        for message in snapshot.messages {
            match message {
                ConversationMessage::User(message) => {
                    markdown.push_str("## 用户\n\n");
                    for part in message.parts {
                        match part {
                            UserPart::Text(text) => {
                                markdown.push_str(text.text.trim_end());
                                markdown.push_str("\n\n");
                            }
                            UserPart::FileReferences(files) if !files.files.is_empty() => {
                                markdown.push_str("附件：\n\n");
                                for file in files.files {
                                    markdown.push_str("- `");
                                    markdown.push_str(&escape_inline_code(&file.original_name));
                                    markdown.push_str("`\n");
                                }
                                markdown.push('\n');
                            }
                            UserPart::Injected(_) | UserPart::FileReferences(_) => {}
                        }
                    }
                }
                ConversationMessage::Assistant(message) => {
                    markdown.push_str("## 助手\n\n");
                    for part in message.parts {
                        match part {
                            AssistantPart::Text(text) => {
                                markdown.push_str(text.text.trim_end());
                                markdown.push_str("\n\n");
                            }
                            AssistantPart::ToolCall(call) => {
                                markdown.push_str("- 工具：`");
                                markdown.push_str(&escape_inline_code(call.name.as_str()));
                                markdown.push_str("`\n");
                            }
                            AssistantPart::Reasoning(_) | AssistantPart::ProviderState(_) => {}
                        }
                    }
                    if !markdown.ends_with("\n\n") {
                        markdown.push('\n');
                    }
                }
                ConversationMessage::Tool(_)
                | ConversationMessage::System(_)
                | ConversationMessage::ContextSummary(_) => {}
            }
        }
        Ok(markdown)
    }

    pub async fn get_application_snapshot(
        &self,
        _request: GetApplicationSnapshotRequest,
    ) -> RuntimeResult<GetApplicationSnapshotResult> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let start = self.event_sender.sequence();
            let configuration = self.get_config_status(Default::default())?.status;
            let models = self.list_models(Default::default())?.models;
            let workspaces = self
                .list_workspaces(ListWorkspacesRequest::default())?
                .workspaces;
            let mut active_sessions = self
                .list_sessions(ListSessionsRequest {
                    filter: SessionListFilter::Active,
                })?
                .sessions;
            let mut archived_sessions = self
                .list_sessions(ListSessionsRequest {
                    filter: SessionListFilter::Archived,
                })?
                .sessions;
            for session in active_sessions
                .iter_mut()
                .chain(archived_sessions.iter_mut())
            {
                session.pending_approval_count =
                    self.approval_registry.list(&session.session_id)?.len() as u64;
                session.active_child_count = self
                    .child_tasks
                    .active_count_for_session(&session.session_id)?;
            }
            let runtime_lifecycle = self.lifecycle()?;
            let end = self.event_sender.sequence();
            if start == end {
                return Ok(GetApplicationSnapshotResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence: end,
                        value: ApplicationSnapshot {
                            runtime_lifecycle,
                            configuration,
                            models,
                            workspaces,
                            active_sessions,
                            archived_sessions,
                            capabilities: ApplicationCapabilities {
                                conversation_paging: true,
                                conversation_search: true,
                                tool_detail: true,
                                queue_control: true,
                                approval_queue: true,
                                child_task_view: true,
                            },
                        },
                    },
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    pub async fn get_session_view(
        &self,
        request: GetSessionViewRequest,
    ) -> RuntimeResult<GetSessionViewResult> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let start = self.event_sender.sequence();
            let session = self.session(&request.session_id)?;
            session
                .ensure_conversation_loaded(self.store.as_ref())
                .await?;
            let mut summary = session.summary()?;
            let approvals = self.approval_queue(&request.session_id)?;
            summary.pending_approval_count = approvals.items.len() as u64;
            summary.active_child_count = self
                .child_tasks
                .active_count_for_session(&request.session_id)?;
            let runs = session.run_snapshots()?;
            let active_run = summary
                .active_run_id
                .as_ref()
                .and_then(|run_id| runs.iter().find(|run| &run.run_id == run_id))
                .cloned();
            let queue = queue_snapshot(&session)?;
            let mut attachments = self
                .list_attachments(ListAttachmentsRequest {
                    session_id: request.session_id.clone(),
                })?
                .attachments;
            let generation = session.lock_state()?.body_generation;
            let projection = self.projection_context(&session, &attachments).await?;
            for attachment in &mut attachments {
                attachment.agent_readable_path.clear();
            }
            let owner = ConversationOwner::MainSession {
                session_id: request.session_id.clone(),
            };
            let window = match self
                .load_product_conversation_window(
                    owner.clone(),
                    generation,
                    None,
                    DEFAULT_PAGE_SIZE,
                )
                .await
            {
                Ok(window) => window,
                Err(RuntimeError::SnapshotStale) => continue,
                Err(error) => return Err(error),
            };
            let conversation = page_from_window(
                owner,
                &window,
                project_conversation(&window.conversation, &projection)?,
            )?;
            let conversation_snapshot = conversation_snapshot_for_usage(&session)?;
            let stored_usage = self
                .store
                .get_session_usage(&request.session_id)
                .await
                .map_err(|source| RuntimeError::from_store("load session usage", source))?;
            let usage = project_usage(&stored_usage, &summary, self)?;
            let file_references = project_conversation_file_references(&conversation_snapshot)?;
            let composer_capabilities = self.composer_capabilities(&summary.model_key)?;
            let child_tasks = self.child_tasks.list_for_session(&request.session_id)?;
            let mut child_task_items = Vec::with_capacity(child_tasks.len());
            for task in child_tasks {
                child_task_items.push(self.child_task_tree_item(task, &approvals.items).await?);
            }
            let end = self.event_sender.sequence();
            if start == end {
                return Ok(GetSessionViewResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence: end,
                        value: SessionViewSnapshot {
                            session: summary,
                            composer_capabilities,
                            active_run,
                            queue,
                            approvals,
                            attachments,
                            file_references,
                            runs,
                            usage,
                            child_tasks: child_task_items,
                            conversation,
                        },
                    },
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    pub async fn get_child_task_view(
        &self,
        request: GetChildTaskViewRequest,
    ) -> RuntimeResult<GetChildTaskViewResult> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let start = self.event_sender.sequence();
            self.session(&request.session_id)?;
            let stored = self
                .child_tasks
                .get(&request.session_id, &request.child_task_id)?
                .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                    session_id: request.session_id.clone(),
                    child_task_id: request.child_task_id.clone(),
                })?;
            let approvals = self.approval_registry.list(&request.session_id)?;
            let task = self
                .child_task_tree_item(stored.clone(), &approvals)
                .await?;
            let owner = ConversationOwner::ChildTask {
                session_id: request.session_id.clone(),
                child_task_id: request.child_task_id.clone(),
            };
            let window = match self
                .load_product_conversation_window(
                    owner.clone(),
                    stored.body_generation,
                    None,
                    DEFAULT_PAGE_SIZE,
                )
                .await
            {
                Ok(window) => window,
                Err(RuntimeError::SnapshotStale) => continue,
                Err(error) => return Err(error),
            };
            let projection = empty_child_projection();
            let conversation = page_from_window(
                owner,
                &window,
                project_conversation(&window.conversation, &projection)?,
            )?;
            let approval_ids = approvals
                .iter()
                .filter(|approval| approval.child_task_id.as_ref() == Some(&request.child_task_id))
                .map(|approval| approval.approval_id.clone())
                .collect();
            let end = self.event_sender.sequence();
            if start == end {
                return Ok(GetChildTaskViewResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence: end,
                        value: ChildTaskViewSnapshot {
                            task,
                            approval_ids,
                            conversation,
                        },
                    },
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    pub async fn list_conversation_page(
        &self,
        request: ListConversationPageRequest,
    ) -> RuntimeResult<ListConversationPageResult> {
        let limit = validated_limit(request.limit)?;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let start = self.event_sender.sequence();
            let (generation, projection) = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    let session = self.session(session_id)?;
                    session
                        .ensure_conversation_loaded(self.store.as_ref())
                        .await?;
                    let attachments = self
                        .list_attachments(ListAttachmentsRequest {
                            session_id: session_id.clone(),
                        })?
                        .attachments;
                    let generation = { session.lock_state()?.body_generation };
                    let projection = self.projection_context(&session, &attachments).await?;
                    (generation, projection)
                }
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => {
                    let task = self
                        .child_tasks
                        .get(session_id, child_task_id)?
                        .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                            session_id: session_id.clone(),
                            child_task_id: child_task_id.clone(),
                        })?;
                    (task.body_generation, empty_child_projection())
                }
            };
            let requested_end = request
                .cursor
                .as_deref()
                .map(decode_cursor)
                .transpose()?
                .map(|cursor| {
                    if cursor.generation != generation {
                        return Err(RuntimeError::SnapshotStale);
                    }
                    Ok(cursor.end)
                })
                .transpose()?;
            let window = match self
                .load_product_conversation_window(
                    request.owner.clone(),
                    generation,
                    requested_end,
                    limit,
                )
                .await
            {
                Ok(window) => window,
                Err(RuntimeError::SnapshotStale) if request.cursor.is_none() => continue,
                Err(error) => return Err(error),
            };
            let page = page_from_window(
                request.owner.clone(),
                &window,
                project_conversation(&window.conversation, &projection)?,
            )?;
            let end = self.event_sender.sequence();
            if start == end {
                return Ok(ListConversationPageResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence: end,
                        value: page,
                    },
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    async fn load_product_conversation_window(
        &self,
        owner: ConversationOwner,
        generation: u64,
        end: Option<usize>,
        limit: usize,
    ) -> RuntimeResult<StoredConversationWindow> {
        let window = self
            .store
            .load_conversation_window(ConversationWindowRequest {
                owner,
                generation,
                end,
                limit,
            })
            .await
            .map_err(|source| {
                if source.kind() == StoreErrorKind::Conflict {
                    RuntimeError::SnapshotStale
                } else {
                    RuntimeError::from_store("load conversation page", source)
                }
            })?;
        if end.is_some_and(|expected| expected != window.end) {
            return Err(RuntimeError::InvalidRequest {
                reason: "conversation cursor is outside the current generation",
            });
        }
        Ok(window)
    }

    pub async fn get_conversation_page_around_run(
        &self,
        request: GetConversationPageAroundRunRequest,
    ) -> RuntimeResult<GetConversationPageAroundRunResult> {
        let limit = validated_limit(request.limit)?;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let start = self.event_sender.sequence();
            let session = self.session(&request.session_id)?;
            session
                .ensure_conversation_loaded(self.store.as_ref())
                .await?;
            session.run_snapshot(&request.run_id)?;
            let attachments = self
                .list_attachments(ListAttachmentsRequest {
                    session_id: request.session_id.clone(),
                })?
                .attachments;
            let generation = session.lock_state()?.body_generation;
            let items = project_conversation(
                &session.conversation_snapshot()?,
                &self.projection_context(&session, &attachments).await?,
            )?;
            let anchor_index = items
                .iter()
                .position(|item| {
                    matches!(item, ConversationItem::Assistant(message) if message.run_id.as_ref() == Some(&request.run_id))
                })
                .ok_or_else(|| RuntimeError::RunNotFound {
                    session_id: request.session_id.clone(),
                    run_id: request.run_id.clone(),
                })?;
            let anchor_message_id = item_message_id(&items[anchor_index]).clone();
            let end_index = (anchor_index + 1).min(items.len());
            let start_index = end_index.saturating_sub(limit);
            let previous_cursor = (start_index > 0)
                .then(|| encode_cursor(generation, start_index))
                .transpose()?;
            let page = ConversationPage {
                owner: ConversationOwner::MainSession {
                    session_id: request.session_id.clone(),
                },
                generation,
                items: items[start_index..end_index].to_vec(),
                previous_cursor,
                has_more: start_index > 0,
            };
            let end = self.event_sender.sequence();
            if start == end {
                return Ok(GetConversationPageAroundRunResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence: end,
                        value: page,
                    },
                    anchor_message_id,
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    /// 查询历史会话标题和正文。正文来自可重建索引，标题来自 Runtime 权威 Session/child 投影。
    pub async fn search_conversation_history(
        &self,
        request: SearchConversationHistoryRequest,
    ) -> RuntimeResult<SearchConversationHistoryResult> {
        let query = request.query.trim();
        if query.is_empty() {
            return Err(RuntimeError::InvalidRequest {
                reason: "conversation search query must not be empty",
            });
        }
        let limit = validated_limit(request.limit)?;
        let offset = usize::try_from(request.offset).map_err(|_| RuntimeError::InvalidRequest {
            reason: "conversation search offset is too large",
        })?;
        let caller = self.session(&request.session_id)?.summary()?;
        let store_scope = match request.scope {
            ConversationHistoryScope::Session => ConversationSearchScope::Session {
                session_id: request.session_id.clone(),
            },
            ConversationHistoryScope::Workspace => ConversationSearchScope::Workspace {
                workspace_id: caller
                    .workspace_id
                    .clone()
                    .ok_or(RuntimeError::InvalidRequest {
                        reason: "workspace search is unavailable for an unbound session",
                    })?,
            },
            ConversationHistoryScope::Global => ConversationSearchScope::Global,
        };
        let mut sessions = self
            .list_sessions(ListSessionsRequest {
                filter: SessionListFilter::Active,
            })?
            .sessions;
        sessions.extend(
            self.list_sessions(ListSessionsRequest {
                filter: SessionListFilter::Archived,
            })?
            .sessions,
        );
        sessions.retain(|session| history_scope_matches(&request, &caller, session));
        let sessions_by_id = sessions
            .iter()
            .map(|session| (session.session_id.as_str().to_owned(), session))
            .collect::<HashMap<_, _>>();
        let normalized_query = query.to_lowercase();
        let mut items = Vec::new();
        for session in &sessions {
            if session.title.to_lowercase().contains(&normalized_query) {
                items.push(ConversationHistoryHit {
                    owner: ConversationOwner::MainSession {
                        session_id: session.session_id.clone(),
                    },
                    session_title: session.title.clone(),
                    child_task_title: None,
                    message_id: None,
                    created_at_ms: session.updated_at_ms,
                    snippet: session.title.clone(),
                    match_kind: ConversationHistoryMatchKind::Title,
                    lifecycle: session.lifecycle,
                });
            }
            for task in self.child_tasks.list_for_session(&session.session_id)? {
                if task.title.to_lowercase().contains(&normalized_query) {
                    items.push(ConversationHistoryHit {
                        owner: ConversationOwner::ChildTask {
                            session_id: session.session_id.clone(),
                            child_task_id: task.child_task_id,
                        },
                        session_title: session.title.clone(),
                        child_task_title: Some(task.title.clone()),
                        message_id: None,
                        created_at_ms: Some(task.finished_at_ms.unwrap_or(task.created_at_ms)),
                        snippet: task.title,
                        match_kind: ConversationHistoryMatchKind::Title,
                        lifecycle: session.lifecycle,
                    });
                }
            }
        }

        let mut partial = false;
        let mut failed_owners = Vec::new();
        if query.chars().count() >= 3 {
            let search_limit = offset
                .saturating_add(limit)
                .saturating_add(1)
                .clamp(1, MAX_PAGE_SIZE);
            let page = self
                .store
                .search_conversations(ConversationSearchRequest {
                    query: query.to_owned(),
                    scope: store_scope,
                    limit: search_limit,
                })
                .await
                .map_err(|source| {
                    RuntimeError::from_store("search conversation history", source)
                })?;
            partial = page.partial;
            failed_owners = page.failed_owners;
            for hit in page.hits {
                let session_id = owner_session_id(&hit.owner);
                let Some(session) = sessions_by_id.get(session_id.as_str()) else {
                    continue;
                };
                let child_task_title = match &hit.owner {
                    ConversationOwner::MainSession { .. } => None,
                    ConversationOwner::ChildTask { child_task_id, .. } => self
                        .child_tasks
                        .get(session_id, child_task_id)?
                        .map(|task| task.title),
                };
                items.push(ConversationHistoryHit {
                    owner: hit.owner,
                    session_title: session.title.clone(),
                    child_task_title,
                    message_id: Some(protocol_message_id(hit.message_id.as_str())?),
                    created_at_ms: Some(hit.created_at_ms),
                    snippet: hit.text,
                    match_kind: ConversationHistoryMatchKind::Message,
                    lifecycle: session.lifecycle,
                });
            }
        }
        items.sort_by_key(|item| std::cmp::Reverse(item.created_at_ms));
        let has_more = items.len() > offset.saturating_add(limit);
        let items = items.into_iter().skip(offset).take(limit).collect();
        Ok(SearchConversationHistoryResult {
            items,
            next_offset: has_more.then(|| request.offset.saturating_add(request.limit)),
            partial,
            failed_owners,
        })
    }

    /// 读取命中附近的有限正文，供搜索结果弹窗预览，不进入 Agent 上下文。
    pub async fn get_conversation_recall_window(
        &self,
        request: GetConversationRecallWindowRequest,
    ) -> RuntimeResult<GetConversationRecallWindowResult> {
        self.session(&request.session_id)?;
        let before = usize::try_from(request.before.min(50)).expect("u32 fits usize");
        let after = usize::try_from(request.after.min(50)).expect("u32 fits usize");
        let (window, projection) = self
            .load_raw_window_around_message(&request.owner, &request.message_id, before, after)
            .await?;
        let items = project_conversation(&window.conversation, &projection)?;
        Ok(GetConversationRecallWindowResult {
            owner: request.owner,
            generation: window.generation,
            anchor_message_id: request.message_id,
            items,
            has_more_before: window.start > 0,
            has_more_after: window.end < window.total,
        })
    }

    /// 按消息 ID 加载有限 Conversation 页，避免为一次定位读取整份历史。
    pub async fn get_conversation_page_around_message(
        &self,
        request: GetConversationPageAroundMessageRequest,
    ) -> RuntimeResult<GetConversationPageAroundMessageResult> {
        let limit = validated_limit(request.limit)?;
        let location = self
            .locate_conversation_message(&request.owner, &request.message_id)
            .await?;
        let display_ordinal = location
            .display_ordinal
            .ok_or(RuntimeError::InvalidRequest {
                reason: "conversation message is not visible in the product conversation",
            })?;
        let requested_end = usize::try_from(display_ordinal)
            .map_err(|_| RuntimeError::InvalidRequest {
                reason: "conversation display position is outside the supported range",
            })?
            .saturating_add(1);
        let projection = self.projection_context_for_owner(&request.owner).await?;
        let window = self
            .load_product_conversation_window(
                request.owner.clone(),
                location.generation,
                Some(requested_end),
                limit,
            )
            .await?;
        let page = page_from_window(
            request.owner.clone(),
            &window,
            project_conversation(&window.conversation, &projection)?,
        )?;
        Ok(GetConversationPageAroundMessageResult {
            snapshot: ObservedSnapshot {
                observed_sequence: self.event_sender.sequence(),
                value: page,
            },
            anchor_message_id: request.message_id,
        })
    }

    async fn load_raw_window_around_message(
        &self,
        owner: &ConversationOwner,
        message_id: &MessageId,
        before: usize,
        after: usize,
    ) -> RuntimeResult<(crate::StoredConversationRawWindow, ProjectionContext)> {
        let location = self.locate_conversation_message(owner, message_id).await?;
        let ordinal = usize::try_from(location.message_ordinal).map_err(|_| {
            RuntimeError::InvalidRequest {
                reason: "conversation message position is outside the supported range",
            }
        })?;
        let start = ordinal.saturating_sub(before);
        let limit = before.saturating_add(after).saturating_add(1).max(1);
        let window = self
            .store
            .load_conversation_raw_window(ConversationRawWindowRequest {
                owner: owner.clone(),
                generation: location.generation,
                start,
                limit,
            })
            .await
            .map_err(|source| {
                RuntimeError::from_store("load conversation recall window", source)
            })?;
        let projection = self.projection_context_for_owner(owner).await?;
        Ok((window, projection))
    }

    async fn locate_conversation_message(
        &self,
        owner: &ConversationOwner,
        message_id: &MessageId,
    ) -> RuntimeResult<crate::StoredConversationMessageLocation> {
        self.store
            .locate_conversation_message(ConversationMessageLocationRequest {
                owner: owner.clone(),
                message_id: agent_types::MessageId::new(message_id.as_str()).map_err(|_| {
                    RuntimeError::InvalidRequest {
                        reason: "conversation message id is invalid",
                    }
                })?,
            })
            .await
            .map_err(|source| RuntimeError::from_store("locate conversation message", source))?
            .ok_or(RuntimeError::InvalidRequest {
                reason: "conversation message no longer exists",
            })
    }

    async fn projection_context_for_owner(
        &self,
        owner: &ConversationOwner,
    ) -> RuntimeResult<ProjectionContext> {
        match owner {
            ConversationOwner::MainSession { session_id } => {
                let session = self.session(session_id)?;
                let attachments = self
                    .list_attachments(ListAttachmentsRequest {
                        session_id: session_id.clone(),
                    })?
                    .attachments;
                Ok(self.projection_context(&session, &attachments).await?)
            }
            ConversationOwner::ChildTask { .. } => Ok(empty_child_projection()),
        }
    }

    pub async fn get_tool_detail(
        &self,
        request: GetToolDetailRequest,
    ) -> RuntimeResult<GetToolDetailResult> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let start = self.event_sender.sequence();
            let (snapshot, run_id) = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    let session = self.session(session_id)?;
                    session
                        .ensure_conversation_loaded(self.store.as_ref())
                        .await?;
                    let run_id = session
                        .lock_state()?
                        .runs
                        .values()
                        .find(|run| {
                            run.message_ids()
                                .iter()
                                .any(|id| id.as_str() == request.message_id.as_str())
                        })
                        .map(|run| run.snapshot().run_id);
                    (session.conversation_snapshot()?, run_id)
                }
                ConversationOwner::ChildTask {
                    session_id,
                    child_task_id,
                } => (
                    self.child_task_conversation_snapshot(session_id, child_task_id)
                        .await?,
                    None,
                ),
            };
            let mut detail = project_tool_detail(
                &snapshot,
                request.owner.clone(),
                &request.message_id,
                &request.call_id,
                run_id,
            )?;
            if detail.tool_name == "recall_memory" {
                detail.recall = self
                    .project_recall_tool_detail(&request.owner, detail.result_json.as_deref())
                    .await;
            }
            let end = self.event_sender.sequence();
            if start == end {
                return Ok(GetToolDetailResult {
                    snapshot: ObservedSnapshot {
                        observed_sequence: end,
                        value: detail,
                    },
                });
            }
        }
        Err(RuntimeError::SnapshotBusy)
    }

    /// 将模型可读的 Recall JSON 转换成桌面可消费的候选列表。
    ///
    /// signed reference 只在 Runtime 内解码与校验；任一旧引用失效时仅禁用该条导航，
    /// 不影响其余正文和工具详情展示。
    async fn project_recall_tool_detail(
        &self,
        caller: &ConversationOwner,
        result_json: Option<&str>,
    ) -> Option<RecallToolDetailSnapshot> {
        let response = serde_json::from_str::<MemoryRecallResponse>(result_json?).ok()?;
        let caller_session_id = match caller {
            ConversationOwner::MainSession { session_id }
            | ConversationOwner::ChildTask { session_id, .. } => session_id,
        };
        let caller_session = self.session(caller_session_id).ok()?;
        let recall = crate::conversation_recall::RuntimeConversationRecall::new(
            self.store.clone(),
            self.recall_reference_codec.clone(),
            caller_session_id.clone(),
            caller_session.environment().workspace_id.clone(),
        );
        let mut items = Vec::with_capacity(response.items.len());
        for item in response.items {
            let navigation = item
                .origins
                .iter()
                .filter_map(|origin| origin.reference.as_deref())
                .find_map(|reference| recall.resolve_reference(reference).ok())
                .and_then(|target| {
                    let session_id = match &target.owner {
                        ConversationOwner::MainSession { session_id }
                        | ConversationOwner::ChildTask { session_id, .. } => session_id,
                    };
                    let lifecycle = self.session(session_id).ok()?.summary().ok()?.lifecycle;
                    Some(RecallNavigationTarget {
                        owner: target.owner,
                        message_id: protocol_message_id(target.message_id.as_str()).ok()?,
                        lifecycle,
                    })
                });
            items.push(RecallToolDetailItem {
                content: item.content,
                role: memory_string_attribute(&item.attributes, "role"),
                created_at_ms: memory_i64_attribute(&item.attributes, "created_at_ms"),
                navigation,
            });
        }
        Some(RecallToolDetailSnapshot {
            items,
            failures: response
                .failures
                .into_iter()
                .map(|failure| RecallToolDetailFailure {
                    source_id: failure.source_id.into_inner(),
                    kind: format!("{:?}", failure.kind).to_lowercase(),
                    message: failure.message,
                })
                .collect(),
            truncated: response.truncated,
        })
    }

    /// 重新从可靠 Conversation 解析工具文件引用，避免 WebView 把任意路径交给 Host。
    pub async fn resolve_tool_file_resource(
        &self,
        owner: &ConversationOwner,
        message_id: &MessageId,
        resource_ref_id: &ResourceRefId,
    ) -> RuntimeResult<ResolvedToolFileResource> {
        let session_id = match owner {
            ConversationOwner::MainSession { session_id }
            | ConversationOwner::ChildTask { session_id, .. } => session_id,
        };
        let session = self.session(session_id)?;
        let (snapshot, environment) = match owner {
            ConversationOwner::MainSession { .. } => {
                session
                    .ensure_conversation_loaded(self.store.as_ref())
                    .await?;
                (session.conversation_snapshot()?, session.environment())
            }
            ConversationOwner::ChildTask { child_task_id, .. } => (
                self.child_task_conversation_snapshot(session_id, child_task_id)
                    .await?,
                session.environment(),
            ),
        };
        let assistant = snapshot.messages.iter().find_map(|message| match message {
            ConversationMessage::Assistant(message)
                if message.id.as_str() == message_id.as_str() =>
            {
                Some(message)
            }
            _ => None,
        });
        let Some(assistant) = assistant else {
            return Err(RuntimeError::InvalidRequest {
                reason: "tool file resource was not found",
            });
        };
        for part in &assistant.parts {
            let AssistantPart::ToolCall(call) = part else {
                continue;
            };
            let result = snapshot.messages.iter().find_map(|message| match message {
                ConversationMessage::Tool(tool) => {
                    (tool.result.call_id.as_str() == call.id.as_str()).then_some(&tool.result)
                }
                _ => None,
            });
            let Some(result) = result.filter(|result| result.status == ToolResultStatus::Success)
            else {
                continue;
            };
            if tool_resource_ref_id(&call.id).as_ref() == Some(resource_ref_id) {
                if call.name.as_str() == "read_image"
                    || matches!(owner, ConversationOwner::ChildTask { .. })
                {
                    continue;
                }
                let path = file_input_path(call.name.as_str(), &call.arguments).ok_or(
                    RuntimeError::InvalidRequest {
                        reason: "tool file resource was not found",
                    },
                )?;
                let path = resolve_recorded_path(&environment.working_directory, path);
                let display_name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or("file")
                    .to_owned();
                return Ok(ResolvedToolFileResource {
                    media_type: preview_media_type(&display_name).map(str::to_owned),
                    path,
                    display_name,
                    origin: ToolFileResourceOrigin::WorkspaceFile,
                    tool_image: None,
                });
            }
            for (part_index, part) in result.content.as_parts().iter().enumerate() {
                let ToolResultPart::Image { image } = part else {
                    continue;
                };
                if tool_image_resource_ref_id(&call.id, part_index).as_ref()
                    != Some(resource_ref_id)
                {
                    continue;
                }
                return Ok(ResolvedToolFileResource {
                    path: std::path::Path::new(&environment.session_tool_image_directory)
                        .join(image.relative_path())
                        .to_string_lossy()
                        .into_owned(),
                    display_name: image.relative_path().to_owned(),
                    media_type: Some(image.media_type().to_owned()),
                    origin: ToolFileResourceOrigin::SessionToolImage,
                    tool_image: Some(image.clone()),
                });
            }
        }
        Err(RuntimeError::InvalidRequest {
            reason: "tool file resource was not found",
        })
    }

    pub(super) fn approval_queue(
        &self,
        session_id: &SessionId,
    ) -> RuntimeResult<ApprovalQueueSnapshot> {
        let items = self.approval_registry.list(session_id)?;
        let resolving_approval_id = items
            .iter()
            .find(|item| item.status == assistant_protocol::ApprovalStatus::Resolving)
            .map(|item| item.approval_id.clone());
        Ok(ApprovalQueueSnapshot {
            revision: self.approval_registry.revision(session_id)?,
            items,
            resolving_approval_id,
        })
    }

    async fn projection_context(
        &self,
        session: &crate::session::SessionController,
        attachments: &[assistant_protocol::AttachmentSummary],
    ) -> RuntimeResult<ProjectionContext> {
        let (run_by_message, input_by_message) = {
            let state = session.lock_state()?;
            let mut run_by_message = HashMap::new();
            for run in state.runs.values() {
                let snapshot = run.snapshot();
                for message_id in run.message_ids() {
                    run_by_message.insert(
                        message_id.as_str().to_owned(),
                        MessageRunProjection {
                            snapshot: snapshot.clone(),
                            finished_at_ms: run.finished_at_ms(),
                        },
                    );
                }
            }
            let input_by_message = state
                .inputs
                .values()
                .map(|input| {
                    (
                        input.stored.user_message_id.as_str().to_owned(),
                        input.stored.input_id.clone(),
                    )
                })
                .collect();
            (run_by_message, input_by_message)
        };
        let attachment_by_path = attachments
            .iter()
            .map(|attachment| {
                (
                    attachment.agent_readable_path.clone(),
                    attachment.attachment_id.clone(),
                )
            })
            .collect();
        let feedback_by_message = self
            .store
            .load_message_feedback(session.id())
            .await
            .map_err(|source| RuntimeError::from_store("load message feedback", source))?
            .into_iter()
            .map(|item| (item.message_id.as_str().to_owned(), item.feedback))
            .collect();
        Ok(ProjectionContext {
            run_by_message,
            input_by_message,
            attachment_by_path,
            feedback_by_message,
            can_fork: true,
        })
    }

    async fn child_task_tree_item(
        &self,
        task: crate::StoredChildTask,
        approvals: &[assistant_protocol::ApprovalSnapshot],
    ) -> RuntimeResult<ChildTaskTreeItemSnapshot> {
        let conversation = self
            .child_task_conversation_snapshot(&task.session_id, &task.child_task_id)
            .await?;
        let snapshot = self.child_snapshot(task).await?;
        let pending_approval_count = approvals
            .iter()
            .filter(|approval| approval.child_task_id.as_ref() == Some(&snapshot.child_task_id))
            .count();
        Ok(ChildTaskTreeItemSnapshot {
            can_cancel: !snapshot.status.is_terminal(),
            task: snapshot,
            usage: project_child_usage(&conversation),
            pending_approval_count: u64::try_from(pending_approval_count).unwrap_or(u64::MAX),
        })
    }
}

impl AssistantRuntime {
    fn composer_capabilities(
        &self,
        model_key: &assistant_protocol::ModelKey,
    ) -> RuntimeResult<ComposerCapabilitiesSnapshot> {
        let snapshot = self.config_registry.snapshot()?;
        let active = snapshot
            .active()
            .ok_or(RuntimeError::ConfigurationUnavailable)?;
        let model = active
            .model(model_key)
            .ok_or_else(|| RuntimeError::ModelUnavailable {
                model_key: model_key.clone(),
            })?;
        let reasoning_effort_options = model
            .capabilities()
            .reasoning
            .as_ref()
            .map(|reasoning| {
                reasoning
                    .efforts
                    .iter()
                    .map(|effort| ReasoningEffortOptionSnapshot {
                        key: protocol_effort_key(effort.key),
                        label: effort.label.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let image_handling = if model.capabilities().image_input {
            ImageHandlingMode::Native
        } else if model.capabilities().tool_calls && active.vision().is_some() {
            ImageHandlingMode::Tool
        } else {
            ImageHandlingMode::Unavailable
        };
        Ok(ComposerCapabilitiesSnapshot {
            reasoning_effort_options,
            image_handling,
        })
    }
}

fn protocol_effort_key(value: crate::ReasoningEffortKey) -> ReasoningEffortKey {
    match value {
        crate::ReasoningEffortKey::Low => ReasoningEffortKey::Low,
        crate::ReasoningEffortKey::Medium => ReasoningEffortKey::Medium,
        crate::ReasoningEffortKey::High => ReasoningEffortKey::High,
        crate::ReasoningEffortKey::XHigh => ReasoningEffortKey::XHigh,
        crate::ReasoningEffortKey::Max => ReasoningEffortKey::Max,
    }
}

fn empty_child_projection() -> ProjectionContext {
    ProjectionContext {
        run_by_message: HashMap::new(),
        input_by_message: HashMap::new(),
        attachment_by_path: HashMap::new(),
        feedback_by_message: HashMap::new(),
        can_fork: false,
    }
}

pub(super) fn queue_snapshot(
    session: &crate::session::SessionController,
) -> RuntimeResult<QueueSnapshot> {
    let state = session.lock_state()?;
    let items = state
        .runnable_inputs
        .iter()
        .enumerate()
        .filter_map(|(position, input_id)| {
            let input = state.inputs.get(input_id)?;
            let text_preview = input
                .stored
                .queued_message
                .as_ref()
                .map(user_text)
                .unwrap_or_default();
            Some(QueuedInputSnapshot {
                input_id: input_id.clone(),
                text_preview: truncate_chars(&text_preview, TOOL_SUMMARY_CHARS),
                submitted_at_ms: input.stored.accepted_at_ms,
                position: u32::try_from(position).unwrap_or(u32::MAX),
                is_prioritized: position == 0,
            })
        })
        .collect();
    let execution_state = if state.queue_paused_by_user {
        QueueExecutionState::PausedByUser
    } else if state.resume_required {
        QueueExecutionState::ResumeRequired
    } else {
        QueueExecutionState::Automatic
    };
    Ok(QueueSnapshot {
        revision: state.queue_revision,
        state: execution_state,
        items,
    })
}

fn project_conversation(
    snapshot: &ConversationSnapshot,
    context: &ProjectionContext,
) -> RuntimeResult<Vec<ConversationItem>> {
    let tool_results = snapshot
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::Tool(tool) => {
                Some((tool.result.call_id.as_str().to_owned(), &tool.result))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut items = Vec::new();
    for message in &snapshot.messages {
        match message {
            ConversationMessage::User(user) => {
                let attachment_ids = user
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::FileReferences(files) => Some(&files.files),
                        _ => None,
                    })
                    .flatten()
                    .filter_map(|file| context.attachment_by_path.get(&file.readable_path).cloned())
                    .collect();
                items.push(ConversationItem::User(UserMessageSnapshot {
                    message_id: protocol_message_id(user.id.as_str())?,
                    input_id: context.input_by_message.get(user.id.as_str()).cloned(),
                    text: user_text(user),
                    attachment_ids,
                    created_at_ms: None,
                }));
            }
            ConversationMessage::Assistant(assistant) => {
                let mut segments = Vec::new();
                let mut pending_tools = Vec::new();
                let flush_tools = |segments: &mut Vec<AssistantSegment>, tools: &mut Vec<_>| {
                    if !tools.is_empty() {
                        segments.push(AssistantSegment::ToolGroup {
                            tools: std::mem::take(tools),
                        });
                    }
                };
                for part in &assistant.parts {
                    match part {
                        AssistantPart::Reasoning(part) => {
                            flush_tools(&mut segments, &mut pending_tools);
                            segments.push(AssistantSegment::Reasoning {
                                part_id: protocol_part_id(part.id.as_str())?,
                                text: part.text.clone(),
                            });
                        }
                        AssistantPart::Text(part) => {
                            flush_tools(&mut segments, &mut pending_tools);
                            segments.push(AssistantSegment::Text {
                                part_id: protocol_part_id(part.id.as_str())?,
                                text: part.text.clone(),
                            });
                        }
                        AssistantPart::ToolCall(call) => {
                            let result = tool_results.get(call.id.as_str()).copied();
                            pending_tools.push(ToolEventSnapshot {
                                call_id: protocol_tool_call_id(call.id.as_str())?,
                                tool_name: call.name.as_str().to_owned(),
                                status: tool_status(result),
                                summary: result.map(tool_result_summary),
                                input: project_tool_input(call.name.as_str(), &call.arguments),
                            });
                        }
                        AssistantPart::ProviderState(_) => {}
                    }
                }
                flush_tools(&mut segments, &mut pending_tools);
                let run = context.run_by_message.get(assistant.id.as_str());
                items.push(ConversationItem::Assistant(AssistantMessageSnapshot {
                    message_id: protocol_message_id(assistant.id.as_str())?,
                    run_id: run.map(|run| run.snapshot.run_id.clone()),
                    attempt: run.map(|run| run.snapshot.attempt),
                    created_at_ms: None,
                    finished_at_ms: run.and_then(|run| run.finished_at_ms),
                    status: run.map(|run| run.snapshot.status),
                    segments,
                    usage: assistant.usage.as_ref().map(token_usage),
                    can_fork: context.can_fork,
                    fork_point: context
                        .can_fork
                        .then(|| protocol_message_id(assistant.id.as_str()))
                        .transpose()?,
                    feedback: context
                        .feedback_by_message
                        .get(assistant.id.as_str())
                        .copied(),
                }));
            }
            ConversationMessage::System(_)
            | ConversationMessage::ContextSummary(_)
            | ConversationMessage::Tool(_) => {}
        }
    }
    Ok(items)
}

fn project_tool_detail(
    snapshot: &ConversationSnapshot,
    owner: ConversationOwner,
    message_id: &MessageId,
    call_id: &ToolCallId,
    run_id: Option<RunId>,
) -> RuntimeResult<ToolDetailSnapshot> {
    let workspace_resources_available = matches!(owner, ConversationOwner::MainSession { .. });
    let assistant = snapshot.messages.iter().find_map(|message| match message {
        ConversationMessage::Assistant(message) if message.id.as_str() == message_id.as_str() => {
            Some(message)
        }
        _ => None,
    });
    let call = assistant
        .and_then(|message| {
            message.parts.iter().find_map(|part| match part {
                AssistantPart::ToolCall(call) if call.id.as_str() == call_id.as_str() => Some(call),
                _ => None,
            })
        })
        .ok_or(RuntimeError::InvalidRequest {
            reason: "tool call was not found in the requested message",
        })?;
    let result = snapshot.messages.iter().find_map(|message| match message {
        ConversationMessage::Tool(tool) if tool.result.call_id.as_str() == call_id.as_str() => {
            Some(&tool.result)
        }
        _ => None,
    });
    let summary = result.map(tool_result_summary);
    let input = project_tool_input(call.name.as_str(), &call.arguments);
    let files = project_tool_files(&call.id, &input, result, workspace_resources_available)?;
    let (request_json, request_truncated) = formatted_json(&call.arguments);
    let (result_json, result_truncated) =
        result
            .and_then(tool_result_json)
            .map_or((None, false), |value| {
                let (formatted, truncated) = formatted_json(&value);
                (Some(formatted), truncated)
            });
    Ok(ToolDetailSnapshot {
        owner,
        message_id: message_id.clone(),
        run_id,
        call_id: call_id.clone(),
        tool_name: call.name.as_str().to_owned(),
        status: tool_status(result),
        input,
        request_json: Some(request_json),
        result_summary: summary,
        result_json,
        recall: None,
        image_inspection: result.and_then(project_image_inspection_detail),
        stdout: None,
        stderr: None,
        error: None,
        files,
        output_truncated: request_truncated || result_truncated,
        historical_fields_missing: result.is_none(),
    })
}

fn project_conversation_file_references(
    snapshot: &ConversationSnapshot,
) -> RuntimeResult<Vec<ConversationFileReference>> {
    let results = snapshot
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::Tool(tool) => Some((tool.result.call_id.as_str(), &tool.result)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut references = Vec::new();
    for message in &snapshot.messages {
        let ConversationMessage::Assistant(assistant) = message else {
            continue;
        };
        let message_id = protocol_message_id(assistant.id.as_str())?;
        for part in &assistant.parts {
            let AssistantPart::ToolCall(call) = part else {
                continue;
            };
            let input = project_tool_input(call.name.as_str(), &call.arguments);
            let files = project_tool_files(
                &call.id,
                &input,
                results.get(call.id.as_str()).copied(),
                true,
            )?;
            let call_id = protocol_tool_call_id(call.id.as_str())?;
            references.extend(
                files
                    .into_iter()
                    .filter(|file| file.origin != ToolFileResourceOrigin::SessionToolImage)
                    .map(|file| ConversationFileReference {
                        message_id: message_id.clone(),
                        call_id: call_id.clone(),
                        file,
                    }),
            );
        }
    }
    Ok(references)
}

fn project_tool_files(
    call_id: &agent_types::ToolCallId,
    input: &ToolInputSnapshot,
    result: Option<&agent_types::ToolResult>,
    workspace_resources_available: bool,
) -> RuntimeResult<Vec<ToolFileReference>> {
    let Some(result) = result else {
        return Ok(Vec::new());
    };
    if result.status != ToolResultStatus::Success {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    if let ToolInputSnapshot::File { operation, path } = input
        && operation != "read_image"
    {
        files.push(ToolFileReference {
            resource_ref_id: tool_resource_ref_id(call_id).ok_or(
                RuntimeError::InternalStateUnavailable {
                    component: "tool resource reference",
                },
            )?,
            origin: ToolFileResourceOrigin::WorkspaceFile,
            display_name: std::path::Path::new(path)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or(path)
                .to_owned(),
            display_path: safe_display_path(path),
            size_bytes: None,
            media_type: preview_media_type(path).map(str::to_owned),
            state: if workspace_resources_available {
                ToolFileResourceState::Available
            } else {
                ToolFileResourceState::Unavailable
            },
        });
    }
    for (part_index, part) in result.content.as_parts().iter().enumerate() {
        let ToolResultPart::Image { image } = part else {
            continue;
        };
        files.push(ToolFileReference {
            resource_ref_id: tool_image_resource_ref_id(call_id, part_index).ok_or(
                RuntimeError::InternalStateUnavailable {
                    component: "tool image resource reference",
                },
            )?,
            origin: ToolFileResourceOrigin::SessionToolImage,
            display_name: image.relative_path().to_owned(),
            display_path: None,
            size_bytes: None,
            media_type: Some(image.media_type().to_owned()),
            state: ToolFileResourceState::Available,
        });
    }
    Ok(files)
}

fn tool_resource_ref_id(call_id: &agent_types::ToolCallId) -> Option<ResourceRefId> {
    ResourceRefId::new(format!("tool-{}", call_id.as_str())).ok()
}

fn tool_image_resource_ref_id(
    call_id: &agent_types::ToolCallId,
    part_index: usize,
) -> Option<ResourceRefId> {
    ResourceRefId::new(format!("tool-image-{}-{part_index}", call_id.as_str())).ok()
}

fn file_input_path<'a>(_name: &str, arguments: &'a serde_json::Value) -> Option<&'a str> {
    arguments.get("path").and_then(serde_json::Value::as_str)
}

fn resolve_recorded_path(working_directory: &str, path: &str) -> String {
    let path = std::path::Path::new(path);
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        std::path::Path::new(working_directory)
            .join(path)
            .to_string_lossy()
            .into_owned()
    }
}

fn preview_media_type(path: &str) -> Option<&'static str> {
    match std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)?
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" | "log" | "rs" | "ts" | "tsx" | "js" | "jsx" | "css" | "scss" | "toml" | "yaml"
        | "yml" | "xml" | "csv" => Some("text/plain"),
        "md" | "markdown" => Some("text/markdown"),
        "json" => Some("application/json"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn safe_display_path(path: &str) -> Option<String> {
    let value = std::path::Path::new(path);
    if value.is_absolute() {
        value
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned)
    } else {
        Some(path.to_owned())
    }
}

fn escape_inline_code(value: &str) -> String {
    value.replace('`', "\\`")
}

fn project_tool_input(name: &str, arguments: &serde_json::Value) -> ToolInputSnapshot {
    if name == "inspect_images" {
        return ToolInputSnapshot::ImageInspection {
            image_paths: arguments
                .get("image_paths")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect(),
            goal: string_field(arguments, "goal"),
            background: arguments
                .get("background")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        };
    }
    if name == "delegate_task" {
        return ToolInputSnapshot::Delegation {
            title: arguments
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            task_summary: arguments
                .get("task")
                .or_else(|| arguments.get("task_summary"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
    }
    if name.contains("shell") {
        return ToolInputSnapshot::Shell {
            command: string_field(arguments, "command"),
            working_directory: string_field(arguments, "working_directory"),
            timeout_ms: arguments
                .get("timeout_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
            process_mode: string_field(arguments, "process_mode"),
        };
    }
    if let Some(path) = arguments.get("path").and_then(serde_json::Value::as_str) {
        return ToolInputSnapshot::File {
            operation: name.to_owned(),
            path: path.to_owned(),
        };
    }
    ToolInputSnapshot::General {
        summary: truncate_chars(&arguments.to_string(), TOOL_SUMMARY_CHARS),
    }
}

fn project_image_inspection_detail(
    result: &agent_types::ToolResult,
) -> Option<assistant_protocol::ImageInspectionDetailSnapshot> {
    let metadata = result.metadata.as_deref()?;
    Some(assistant_protocol::ImageInspectionDetailSnapshot {
        auxiliary_model: assistant_protocol::ModelKey::new(metadata.model_key.clone()?).ok()?,
        elapsed_ms: metadata.elapsed_ms?,
        usage: metadata.usage.as_ref().map(token_usage),
    })
}

fn project_usage(
    stored: &crate::StoredSessionUsage,
    session: &assistant_protocol::SessionSummary,
    runtime: &AssistantRuntime,
) -> RuntimeResult<SessionUsageSnapshot> {
    let previous = stored.latest.as_ref();
    let accumulated = (stored.request_count > 0).then_some(UsageTotals {
        input_tokens: Some(stored.input_tokens),
        output_tokens: Some(stored.output_tokens),
        total_tokens: Some(stored.total_tokens),
        cached_input_tokens: (stored.cached_request_count > 0)
            .then_some(stored.cached_input_tokens),
    });
    let latest_cache_hit_basis_points = previous.and_then(cache_hit_basis_points);
    let overall_cache_hit_basis_points = (stored.request_count > 0
        && stored.cached_request_count == stored.request_count)
        .then(|| ratio_basis_points(stored.cached_input_tokens, stored.input_tokens))
        .flatten();
    let context_window = runtime
        .list_models(Default::default())?
        .models
        .into_iter()
        .find(|model| model.model_key.as_ref() == Some(&session.model_key))
        .and_then(|model| model.context_window_tokens);
    let context = previous.zip(context_window).map(|(usage, window_tokens)| {
        let used_tokens = usage.input_tokens;
        let basis_points = used_tokens
            .saturating_mul(10_000)
            .checked_div(window_tokens.max(1))
            .unwrap_or_default()
            .min(10_000);
        assistant_protocol::ContextUsageSnapshot {
            used_tokens,
            window_tokens,
            usage_basis_points: u16::try_from(basis_points).unwrap_or(10_000),
        }
    });
    Ok(SessionUsageSnapshot {
        accumulated,
        previous_turn: previous.map(usage_totals),
        latest_cache_hit_basis_points,
        overall_cache_hit_basis_points,
        context,
    })
}

fn project_child_usage(snapshot: &ConversationSnapshot) -> ChildTaskUsageSnapshot {
    let usages = snapshot.messages.iter().flat_map(|message| match message {
        ConversationMessage::Assistant(message) => message.usage.iter().collect::<Vec<_>>(),
        ConversationMessage::ContextSummary(message) => message
            .compacted_usage
            .iter()
            .chain(message.usage.iter())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    });
    ChildTaskUsageSnapshot {
        accumulated: sum_usage(usages),
    }
}

fn conversation_snapshot_for_usage(
    session: &crate::session::SessionController,
) -> RuntimeResult<ConversationSnapshot> {
    session.conversation_snapshot()
}

fn sum_usage<'a>(usages: impl Iterator<Item = &'a agent_types::TokenUsage>) -> Option<UsageTotals> {
    let mut found = false;
    let mut total = UsageTotals {
        input_tokens: Some(0),
        output_tokens: Some(0),
        total_tokens: Some(0),
        cached_input_tokens: None,
    };
    let mut cached = 0_u64;
    let mut has_cached = false;
    for usage in usages {
        found = true;
        total.input_tokens = total
            .input_tokens
            .map(|value| value.saturating_add(usage.input_tokens));
        total.output_tokens = total
            .output_tokens
            .map(|value| value.saturating_add(usage.output_tokens));
        total.total_tokens = total
            .total_tokens
            .map(|value| value.saturating_add(usage.total_tokens));
        if let Some(value) = usage.cached_input_tokens {
            has_cached = true;
            cached = cached.saturating_add(value);
        }
    }
    total.cached_input_tokens = has_cached.then_some(cached);
    found.then_some(total)
}

fn usage_totals(usage: &agent_types::TokenUsage) -> UsageTotals {
    UsageTotals {
        input_tokens: Some(usage.input_tokens),
        output_tokens: Some(usage.output_tokens),
        total_tokens: Some(usage.total_tokens),
        cached_input_tokens: usage.cached_input_tokens,
    }
}

fn cache_hit_basis_points(usage: &agent_types::TokenUsage) -> Option<u16> {
    let cached = usage.cached_input_tokens?;
    ratio_basis_points(cached, usage.input_tokens)
}

fn ratio_basis_points(numerator: u64, denominator: u64) -> Option<u16> {
    if denominator == 0 {
        return None;
    }
    let basis_points =
        u128::from(numerator.min(denominator)).saturating_mul(10_000) / u128::from(denominator);
    u16::try_from(basis_points).ok()
}

fn token_usage(usage: &agent_types::TokenUsage) -> TokenUsageSnapshot {
    TokenUsageSnapshot {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cached_input_tokens: usage.cached_input_tokens,
    }
}

fn page_from_window(
    owner: ConversationOwner,
    window: &StoredConversationWindow,
    items: Vec<ConversationItem>,
) -> RuntimeResult<ConversationPage> {
    if items.len() != window.end.saturating_sub(window.start) {
        return Err(RuntimeError::InternalStateUnavailable {
            component: "conversation page display boundaries",
        });
    }
    let previous_cursor = (window.start > 0)
        .then(|| encode_cursor(window.generation, window.start))
        .transpose()?;
    Ok(ConversationPage {
        owner,
        generation: window.generation,
        items,
        previous_cursor,
        has_more: window.start > 0,
    })
}

fn validated_limit(limit: u32) -> RuntimeResult<usize> {
    if limit == 0 || usize::try_from(limit).unwrap_or(usize::MAX) > MAX_PAGE_SIZE {
        return Err(RuntimeError::InvalidRequest {
            reason: "conversation page limit must be between 1 and 100",
        });
    }
    Ok(usize::try_from(limit).unwrap_or(MAX_PAGE_SIZE))
}

/// 产品搜索范围在进入派生索引前先由 Runtime 权威 Session 投影收窄。
fn history_scope_matches(
    request: &SearchConversationHistoryRequest,
    caller: &assistant_protocol::SessionSummary,
    candidate: &assistant_protocol::SessionSummary,
) -> bool {
    match request.scope {
        ConversationHistoryScope::Session => candidate.session_id == request.session_id,
        ConversationHistoryScope::Workspace => {
            caller.workspace_id.is_some() && candidate.workspace_id == caller.workspace_id
        }
        ConversationHistoryScope::Global => true,
    }
}

fn owner_session_id(owner: &ConversationOwner) -> &SessionId {
    match owner {
        ConversationOwner::MainSession { session_id }
        | ConversationOwner::ChildTask { session_id, .. } => session_id,
    }
}

fn encode_cursor(generation: u64, end: usize) -> RuntimeResult<String> {
    serde_json::to_vec(&ConversationCursor { generation, end })
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "conversation cursor encoder",
        })
}

fn decode_cursor(cursor: &str) -> RuntimeResult<ConversationCursor> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(RuntimeError::InvalidRequest {
            reason: "conversation cursor is invalid",
        })
}

fn user_text(message: &agent_types::UserMessage) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            UserPart::Text(text) => Some(text.text.as_str()),
            UserPart::Injected(_) | UserPart::FileReferences(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_status(result: Option<&agent_types::ToolResult>) -> ToolActivityStatus {
    match result.map(|result| &result.status) {
        None => ToolActivityStatus::Proposed,
        Some(ToolResultStatus::Success) => ToolActivityStatus::Completed,
        Some(ToolResultStatus::Error) => ToolActivityStatus::Failed,
    }
}

fn tool_result_summary(result: &agent_types::ToolResult) -> String {
    let text = result
        .content
        .as_parts()
        .iter()
        .map(|part| match part {
            agent_types::ToolResultPart::Text { text } => text.clone(),
            agent_types::ToolResultPart::Json { value } => value.to_string(),
            agent_types::ToolResultPart::Image { image } => {
                format!("[image: {}]", image.relative_path())
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate_chars(&text, TOOL_SUMMARY_CHARS)
}

fn tool_result_json(result: &agent_types::ToolResult) -> Option<serde_json::Value> {
    result.content.as_single_json().cloned().or_else(|| {
        result
            .content
            .as_single_text()
            .and_then(|text| serde_json::from_str(text).ok())
    })
}

fn formatted_json(value: &serde_json::Value) -> (String, bool) {
    let formatted = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let mut chars = formatted.chars();
    let retained = chars
        .by_ref()
        .take(TOOL_DETAIL_JSON_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        (format!("{retained}\n…"), true)
    } else {
        (retained, false)
    }
}

fn memory_string_attribute(
    attributes: &std::collections::BTreeMap<String, MemoryPropertyValue>,
    key: &str,
) -> Option<String> {
    match attributes.get(key) {
        Some(MemoryPropertyValue::String(value)) => Some(value.clone()),
        Some(MemoryPropertyValue::Number(_)) | None => None,
    }
}

fn memory_i64_attribute(
    attributes: &std::collections::BTreeMap<String, MemoryPropertyValue>,
    key: &str,
) -> Option<i64> {
    match attributes.get(key) {
        Some(MemoryPropertyValue::Number(value)) => value.as_i64(),
        Some(MemoryPropertyValue::String(_)) | None => None,
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn string_field(value: &serde_json::Value, field: &str) -> String {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn protocol_message_id(value: &str) -> RuntimeResult<MessageId> {
    MessageId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "conversation message id",
    })
}

fn protocol_part_id(value: &str) -> RuntimeResult<PartId> {
    PartId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "conversation part id",
    })
}

fn protocol_tool_call_id(value: &str) -> RuntimeResult<ToolCallId> {
    ToolCallId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "conversation tool call id",
    })
}

fn item_message_id(item: &ConversationItem) -> &MessageId {
    match item {
        ConversationItem::User(message) => &message.message_id,
        ConversationItem::Assistant(message) => &message.message_id,
    }
}

#[cfg(test)]
mod tool_image_projection_tests {
    use agent_types::{
        AssistantMessage, FinishReason, ModelIdentity, ProviderId, ToolCall, ToolMessage, ToolName,
        ToolResult, ToolResultContent,
    };
    use serde_json::json;

    use super::*;

    fn image_exchange() -> (
        ConversationSnapshot,
        agent_types::MessageId,
        agent_types::ToolCallId,
    ) {
        let assistant_id = agent_types::MessageId::new("assistant-image").expect("assistant id");
        let call_id = agent_types::ToolCallId::new("read-image-call").expect("call id");
        let image = ToolImageReference::new(format!("{}.png", "a".repeat(64)), "image/png")
            .expect("image reference");
        let assistant = AssistantMessage {
            id: assistant_id.clone(),
            model: ModelIdentity::new(
                ProviderId::new("fixture").expect("provider"),
                "fixture-model",
            ),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: call_id.clone(),
                name: ToolName::new("read_image").expect("tool name"),
                arguments: json!({"path": "/outside/source.png"}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        };
        let tool = ToolMessage {
            id: agent_types::MessageId::new("tool-image-result").expect("tool message id"),
            result: ToolResult {
                call_id: call_id.clone(),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![
                    ToolResultPart::text("copied"),
                    ToolResultPart::image(image),
                ])
                .expect("tool result content"),
                metadata: None,
            },
        };
        (
            ConversationSnapshot::new(vec![
                ConversationMessage::Assistant(assistant),
                ConversationMessage::Tool(tool),
            ]),
            assistant_id,
            call_id,
        )
    }

    #[test]
    fn read_image_projects_only_the_persisted_image_part() {
        let (snapshot, assistant_id, call_id) = image_exchange();
        let detail = project_tool_detail(
            &snapshot,
            ConversationOwner::ChildTask {
                session_id: SessionId::new("session-image").expect("session id"),
                child_task_id: assistant_protocol::ChildTaskId::new("child-image")
                    .expect("child id"),
            },
            &MessageId::new(assistant_id.as_str()).expect("protocol message id"),
            &ToolCallId::new(call_id.as_str()).expect("protocol call id"),
            None,
        )
        .expect("tool detail");

        assert_eq!(detail.files.len(), 1);
        assert_eq!(
            detail.files[0].resource_ref_id.as_str(),
            "tool-image-read-image-call-1"
        );
        assert_eq!(
            detail.files[0].origin,
            ToolFileResourceOrigin::SessionToolImage
        );
        assert_eq!(detail.files[0].display_path, None);
        assert_eq!(detail.files[0].media_type.as_deref(), Some("image/png"));
        assert_eq!(detail.files[0].state, ToolFileResourceState::Available);
    }

    #[test]
    fn tool_images_never_enter_conversation_file_references() {
        let (snapshot, _, _) = image_exchange();
        assert!(
            project_conversation_file_references(&snapshot)
                .expect("conversation file projection")
                .is_empty()
        );
    }
}
