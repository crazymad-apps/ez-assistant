//! Desktop 使用的组合快照、分页 Conversation 与安全工具详情投影。

use std::collections::HashMap;

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ToolResultContent, ToolResultStatus,
    UserPart,
};
use assistant_protocol::{
    ApplicationCapabilities, ApplicationSnapshot, ApprovalQueueSnapshot, AssistantMessageSnapshot,
    AssistantSegment, AttachmentId, ChildTaskTreeItemSnapshot, ChildTaskUsageSnapshot,
    ChildTaskViewSnapshot, ConversationFileReference, ConversationItem, ConversationOwner,
    ConversationPage, GetApplicationSnapshotRequest, GetApplicationSnapshotResult,
    GetChildTaskViewRequest, GetChildTaskViewResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, ListAttachmentsRequest, ListConversationPageRequest,
    ListConversationPageResult, ListSessionsRequest, ListWorkspacesRequest, MessageId,
    ObservedSnapshot, PartId, QueueExecutionState, QueueSnapshot, QueuedInputSnapshot,
    ResourceRefId, RunId, RunSnapshot, SessionId, SessionListFilter, SessionUsageSnapshot,
    SessionViewSnapshot, TokenUsageSnapshot, ToolActivityStatus, ToolCallId, ToolDetailSnapshot,
    ToolEventSnapshot, ToolFileReference, ToolFileResourceOrigin, ToolFileResourceState,
    ToolInputSnapshot, UsageTotals, UserMessageSnapshot,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use super::AssistantRuntime;
use crate::{
    ConversationWindowRequest, RuntimeError, RuntimeResult, StoreErrorKind,
    StoredConversationWindow,
};

const DEFAULT_PAGE_SIZE: usize = 30;
const MAX_PAGE_SIZE: usize = 100;
const SNAPSHOT_ATTEMPTS: usize = 4;
const TOOL_SUMMARY_CHARS: usize = 160;

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
            let usage = project_usage(&conversation_snapshot, &summary, self)?;
            let file_references = project_conversation_file_references(&conversation_snapshot)?;
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
            let detail = project_tool_detail(
                &snapshot,
                request.owner.clone(),
                &request.message_id,
                &request.call_id,
                run_id,
            )?;
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
        let (snapshot, working_directory) = match owner {
            ConversationOwner::MainSession { .. } => {
                session
                    .ensure_conversation_loaded(self.store.as_ref())
                    .await?;
                (
                    session.conversation_snapshot()?,
                    session.environment().working_directory.clone(),
                )
            }
            ConversationOwner::ChildTask { .. } => {
                return Err(RuntimeError::InvalidRequest {
                    reason: "child task file resources are no longer available",
                });
            }
        };
        let assistant = snapshot.messages.iter().find_map(|message| match message {
            ConversationMessage::Assistant(message)
                if message.id.as_str() == message_id.as_str() =>
            {
                Some(message)
            }
            _ => None,
        });
        let Some((call, path)) = assistant.and_then(|message| {
            message.parts.iter().find_map(|part| match part {
                AssistantPart::ToolCall(call)
                    if tool_resource_ref_id(&call.id).as_ref() == Some(resource_ref_id) =>
                {
                    file_input_path(call.name.as_str(), &call.arguments).map(|path| (call, path))
                }
                _ => None,
            })
        }) else {
            return Err(RuntimeError::InvalidRequest {
                reason: "tool file resource was not found",
            });
        };
        let completed = snapshot.messages.iter().any(|message| match message {
            ConversationMessage::Tool(tool) => {
                tool.result.call_id.as_str() == call.id.as_str()
                    && tool.result.status == ToolResultStatus::Success
            }
            _ => false,
        });
        if !completed {
            return Err(RuntimeError::InvalidRequest {
                reason: "tool file resource is not available",
            });
        }
        let path = resolve_recorded_path(&working_directory, path);
        let display_name = std::path::Path::new(&path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("file")
            .to_owned();
        Ok(ResolvedToolFileResource { path, display_name })
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
    let resources_available = matches!(owner, ConversationOwner::MainSession { .. });
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
    let files = project_tool_files(&call.id, &input, result, resources_available)?;
    Ok(ToolDetailSnapshot {
        owner,
        message_id: message_id.clone(),
        run_id,
        call_id: call_id.clone(),
        tool_name: call.name.as_str().to_owned(),
        status: tool_status(result),
        input,
        result_summary: summary,
        stdout: None,
        stderr: None,
        error: None,
        files,
        output_truncated: false,
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
            references.extend(files.into_iter().map(|file| ConversationFileReference {
                message_id: message_id.clone(),
                call_id: call_id.clone(),
                file,
            }));
        }
    }
    Ok(references)
}

fn project_tool_files(
    call_id: &agent_types::ToolCallId,
    input: &ToolInputSnapshot,
    result: Option<&agent_types::ToolResult>,
    resources_available: bool,
) -> RuntimeResult<Vec<ToolFileReference>> {
    let (ToolInputSnapshot::File { path, .. }, Some(result)) = (input, result) else {
        return Ok(Vec::new());
    };
    if result.status != ToolResultStatus::Success {
        return Ok(Vec::new());
    }
    Ok(vec![ToolFileReference {
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
        state: if resources_available {
            ToolFileResourceState::Available
        } else {
            ToolFileResourceState::Unavailable
        },
    }])
}

fn tool_resource_ref_id(call_id: &agent_types::ToolCallId) -> Option<ResourceRefId> {
    ResourceRefId::new(format!("tool-{}", call_id.as_str())).ok()
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

fn project_usage(
    snapshot: &ConversationSnapshot,
    session: &assistant_protocol::SessionSummary,
    runtime: &AssistantRuntime,
) -> RuntimeResult<SessionUsageSnapshot> {
    let mut usages = Vec::new();
    let mut compacted = Vec::new();
    for message in &snapshot.messages {
        match message {
            ConversationMessage::Assistant(message) => {
                usages.extend(message.usage.as_ref());
            }
            ConversationMessage::ContextSummary(message) => {
                usages.extend(message.usage.as_ref());
                compacted.extend(message.compacted_usage.as_ref());
            }
            _ => {}
        }
    }
    let previous = usages.last().copied();
    let all = compacted.into_iter().chain(usages.iter().copied());
    let accumulated = sum_usage(all);
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
    let text = match &result.content {
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Json(value) => value.to_string(),
    };
    truncate_chars(&text, TOOL_SUMMARY_CHARS)
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
