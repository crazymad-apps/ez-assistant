//! Desktop 使用的组合快照、分页 Conversation 与安全工具详情投影。

#[cfg(test)]
#[path = "product/history_tests.rs"]
mod history_tests;

use std::collections::HashMap;

use agent_memory::{MemoryPropertyValue, MemoryRecallResponse};
use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ToolImageReference, ToolResultPart,
    ToolResultStatus, UserPart,
};
use assistant_protocol::{
    ActiveSkillSnapshot, ApplicationCapabilities, ApplicationSnapshot, ApprovalQueueSnapshot,
    AssistantMessageSnapshot, AssistantSegment, AttachmentId, ChildTaskTreeItemSnapshot,
    ChildTaskUsageSnapshot, ChildTaskViewSnapshot, ComposerCapabilitiesSnapshot,
    ConversationFileReference, ConversationHistoryHit, ConversationHistoryMatchKind,
    ConversationHistoryScope, ConversationInputSourceSnapshot, ConversationItem, ConversationOwner,
    ConversationPage, GetApplicationSnapshotRequest, GetApplicationSnapshotResult,
    GetChildTaskViewRequest, GetChildTaskViewResult, GetConversationPageAroundMessageRequest,
    GetConversationPageAroundMessageResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetConversationRecallWindowRequest,
    GetConversationRecallWindowResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, GoalBudgetSnapshot, GoalPauseReasonSnapshot,
    GoalSnapshot, GoalStateSnapshot, ImageHandlingMode, ListAttachmentsRequest,
    ListConversationPageRequest, ListConversationPageResult, ListSessionsRequest,
    ListWorkspacesRequest, McpToolIdentity, MessageId, ObservedSnapshot, PartId,
    QueueExecutionState, QueueSnapshot, QueuedInputSnapshot, QueuedSessionCommandSnapshot,
    QueuedSessionItemSnapshot, ReasoningEffortKey, ReasoningEffortOptionSnapshot,
    RecallNavigationTarget, RecallToolDetailFailure, RecallToolDetailItem,
    RecallToolDetailSnapshot, ResourceRefId, RunId, SearchConversationHistoryRequest,
    SearchConversationHistoryResult, SessionId, SessionListFilter, SessionUsageSnapshot,
    SessionViewSnapshot, SkillActivationTagSnapshot, SkillActivationTriggerSnapshot,
    SkillDiagnosticSeveritySnapshot, SkillDiagnosticSnapshot, SkillHealthSnapshot,
    SkillManagementSnapshot, SkillSourceSnapshot, SkillSummarySnapshot, TodoItemStatusSnapshot,
    TokenUsageSnapshot, ToolActivityStatus, ToolCallId, ToolDetailSnapshot, ToolEventSnapshot,
    ToolFileReference, ToolFileResourceOrigin, ToolFileResourceState, ToolInputSnapshot,
    UsageTotals, UserMessageSnapshot, WorkPlanItemSnapshot, WorkPlanSnapshot,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use super::AssistantRuntime;
use super::tool_input_projection::{project_json, project_tool_input};
use crate::observation::SharedSnapshotSources;
use crate::{
    ConversationMessageLocationRequest, ConversationRawWindowRequest, ConversationSearchRequest,
    ConversationSearchScope, ConversationWindowRequest, CrossSessionInputBinding,
    InputChannelSource, InputModality, OutputPreference, RuntimeError, RuntimeResult,
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

#[derive(Clone)]
pub(super) struct ProjectionContext {
    device_names: HashMap<assistant_protocol::DeviceId, String>,
    run_by_message: HashMap<String, MessageRunProjection>,
    input_by_message: HashMap<String, assistant_protocol::InputId>,
    source_by_message: HashMap<String, ConversationInputSourceSnapshot>,
    skill_by_message: HashMap<String, SkillActivationTagSnapshot>,
    mcp_selection_by_message: HashMap<String, assistant_protocol::McpSelectionTagSnapshot>,
    control_result_by_message: HashMap<String, crate::StoredSessionCommandResult>,
    attachment_by_path: HashMap<String, AttachmentId>,
    feedback_by_message: HashMap<String, assistant_protocol::MessageFeedback>,
    mcp_identities: HashMap<(String, String), McpToolIdentity>,
    can_fork: bool,
}

#[derive(Clone)]
struct MessageRunProjection {
    inherited: bool,
    partial: bool,
    shell_switch: Option<(assistant_protocol::ShellKind, bool)>,
    run_id: Option<RunId>,
    attempt: Option<u32>,
    created_at_ms: Option<i64>,
    started_at_ms: Option<i64>,
    status: Option<assistant_protocol::RunStatus>,
    error: Option<assistant_protocol::RuntimeErrorInfo>,
    finished_at_ms: Option<i64>,
    step: Option<u32>,
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
        let session = self.session(session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let snapshot = self
            .store
            .load_conversation(session_id)
            .await
            .map_err(|source| RuntimeError::from_store("load export conversation", source))?;
        let mut markdown = String::new();
        let title = session.summary()?.title;
        markdown.push_str("# ");
        markdown.push_str(&title);
        markdown.push_str("\n\n");
        for message in snapshot.messages {
            match message {
                ConversationMessage::User(message)
                    if message.transcript_visibility.is_visible() =>
                {
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
                            UserPart::Injected(_)
                            | UserPart::InternalContext(_)
                            | UserPart::QuotedText(_)
                            | UserPart::FileReferences(_) => {}
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
                ConversationMessage::User(_)
                | ConversationMessage::Tool(_)
                | ConversationMessage::System(_)
                | ConversationMessage::ContextSummary(_) => {}
            }
        }
        Ok(markdown)
    }

    pub async fn get_application_snapshot(
        &self,
        request: GetApplicationSnapshotRequest,
    ) -> RuntimeResult<GetApplicationSnapshotResult> {
        let _projection = crate::request_diagnostics::request_phase(
            crate::request_diagnostics::RequestPhase::Projection,
        );
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_application();
            let configuration = self.get_config_status(Default::default())?.status;
            let (providers, model_settings) = self.model_management_snapshot()?;
            let workspaces = self
                .list_workspaces(ListWorkspacesRequest::default())?
                .workspaces;
            let active_page = self
                .list_sessions(ListSessionsRequest {
                    filter: SessionListFilter::Active,
                    limit: request.active_sessions_limit,
                    ..Default::default()
                })
                .await?;
            let archived_page = self
                .list_sessions(ListSessionsRequest {
                    filter: SessionListFilter::Archived,
                    limit: request.archived_sessions_limit,
                    ..Default::default()
                })
                .await?;
            // 分页位置按实际查询前缀计算，先于可见工作区过滤和固定主控入口补入。
            let active_sessions_next_offset = active_page
                .has_more
                .then_some(active_page.sessions.len() as u32);
            let archived_sessions_next_offset = archived_page
                .has_more
                .then_some(archived_page.sessions.len() as u32);
            let mut active_sessions = active_page.sessions;
            let mut archived_sessions = archived_page.sessions;
            let active_workspace_ids = workspaces
                .iter()
                .map(|workspace| workspace.workspace_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let belongs_to_visible_workspace = |session: &assistant_protocol::SessionSummary| {
                session
                    .workspace_id
                    .as_ref()
                    .is_none_or(|workspace_id| active_workspace_ids.contains(workspace_id))
            };
            active_sessions.retain(&belongs_to_visible_workspace);
            archived_sessions.retain(belongs_to_visible_workspace);
            let runtime_lifecycle = self.lifecycle()?;
            let controllers = self.controller_summaries().await?;
            // 主控是固定入口，不受普通会话首页分页截断。
            if let Some(controller) = controllers.first()
                && !active_sessions
                    .iter()
                    .any(|row| row.session_id == controller.session_id)
            {
                active_sessions.push(controller.clone());
            }
            let controller_availability = controllers
                .first()
                .map(
                    |controller| assistant_protocol::ControllerAvailabilitySnapshot::Available {
                        session_id: controller.session_id.clone(),
                    },
                )
                .unwrap_or_default();
            let additional_controller_count = controllers.len().saturating_sub(1) as u64;
            let mut approval_revisions = Vec::new();
            for session in active_sessions
                .iter_mut()
                .chain(archived_sessions.iter_mut())
            {
                let (approvals, revision) = self
                    .approval_registry
                    .list_with_revision(&session.session_id)?;
                session.pending_approval_count = approvals.len() as u64;
                approval_revisions.push((session.session_id.clone(), revision));
                session.active_child_count = self
                    .child_tasks
                    .active_count_for_session(&session.session_id)?;
            }
            let mcp_tools = !self.mcp_service.registry.server_keys()?.is_empty();
            if let Some((end, approvals_unchanged)) = observation.capture(|| {
                for (session_id, revision) in &approval_revisions {
                    if self.approval_registry.revision(session_id)? != *revision {
                        return Ok(false);
                    }
                }
                Ok(true)
            })? && approvals_unchanged
            {
                return Ok(GetApplicationSnapshotResult {
                    snapshot: ObservedSnapshot {
                        observed_at_ms: super::now_ms().ok(),
                        observed_sequence: end,
                        value: ApplicationSnapshot {
                            runtime_lifecycle,
                            configuration,
                            providers,
                            model_settings,
                            workspaces,
                            active_sessions,
                            archived_sessions,
                            active_sessions_next_offset,
                            archived_sessions_next_offset,
                            controller_availability,
                            additional_controller_count,
                            capabilities: ApplicationCapabilities {
                                conversation_paging: true,
                                conversation_search: true,
                                tool_detail: true,
                                queue_control: true,
                                approval_queue: true,
                                child_task_view: true,
                                mcp_tools,
                                mcp_management: self.mcp_service.management_available(),
                                session_commands: true,
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
        let _projection = crate::request_diagnostics::request_phase(
            crate::request_diagnostics::RequestPhase::Projection,
        );
        let session = self.prepared_session(&request.session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        {
            let state = session.lock_state()?;
            if state.journal.is_none() && !state.is_conversation_available {
                return Err(RuntimeError::StorageUnavailable {
                    operation: "load session conversation",
                    source: None,
                });
            }
        }
        'snapshot: for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_session_view(
                &request.session_id,
                session.environment().workspace_id.clone(),
            );
            let configuration = self.config_registry.snapshot()?;
            let device_names = self.device_names()?;
            let workspace = self.session_workspace_snapshot(&session)?;
            let mut summary = session.summary()?;
            let approvals = self.approval_queue(&request.session_id)?;
            summary.pending_approval_count = approvals.items.len() as u64;
            summary.active_child_count = self
                .child_tasks
                .active_count_for_session(&request.session_id)?;
            let conversation_snapshot = session
                .lock_state()?
                .journal
                .as_ref()
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "session execution window",
                })?
                .snapshot();

            let run_history = self
                .store
                .load_conversation_history(ConversationOwner::MainSession {
                    session_id: request.session_id.clone(),
                })
                .await
                .map_err(|source| RuntimeError::from_store("load session Run history", source))?;
            let mut runs =
                self.project_product_run_snapshots(&session, &run_history, &conversation_snapshot)?;
            let queue = queue_snapshot(&session, &device_names)?;
            let mut attachments = self
                .list_attachments(ListAttachmentsRequest {
                    session_id: request.session_id.clone(),
                })
                .await?
                .attachments;
            let generation = session.lock_state()?.body_generation;
            let projection = self
                .projection_context_with_devices(&session, &attachments, device_names)
                .await?;
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
                project_conversation_with_history(
                    &window.conversation,
                    window.history.as_ref(),
                    &projection,
                )?,
            )?;
            let stored_usage = self
                .store
                .get_session_usage(&request.session_id)
                .await
                .map_err(|source| RuntimeError::from_store("load session usage", source))?;
            let usage_snapshot = self
                .store
                .get_conversation_usage(ConversationOwner::MainSession {
                    session_id: request.session_id.clone(),
                })
                .await
                .map_err(|source| {
                    RuntimeError::from_store("load context usage snapshot", source)
                })?;
            let (composer_capabilities, context_window) = match self
                .composer_capabilities(&configuration, summary.model_selection.as_ref())
                .await
            {
                Ok(value) => value,
                Err(RuntimeError::SnapshotStale) => continue,
                Err(error) => return Err(error),
            };
            let mode = session
                .lock_state()?
                .context_management_mode
                .unwrap_or_default();
            let mut usage = project_usage(
                &stored_usage,
                context_window,
                usage_snapshot.used_tokens(),
                mode,
            );
            if let Some(context) = &mut usage.context {
                context.calibration_pending = usage_snapshot.calibration_pending.then_some(true);
            }
            apply_current_context_measurement(
                &mut usage.context,
                &session,
                "",
                &conversation_snapshot,
            )?;
            let (file_references, artifacts) = session
                .lock_state()?
                .journal
                .as_ref()
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "session display metadata",
                })?
                .display_metadata();
            let (work_plan, goal, active_skills, agent_shell_kind) = {
                let state = session.lock_state()?;
                (
                    state.work_plan.as_ref().map(project_work_plan),
                    state.goal.as_ref().map(project_goal).transpose()?,
                    project_active_skills(&state)?,
                    state.agent_shell_kind,
                )
            };
            let title_generation = session
                .lock_state()?
                .active_title_generation
                .as_ref()
                .map(|active| active.snapshot.clone());
            let child_tasks = self.product_child_tasks(&request.session_id).await?;
            let mut child_task_items = Vec::with_capacity(child_tasks.len());
            for task in child_tasks {
                match self
                    .child_task_tree_summary(&request.session_id, task, &approvals.items)
                    .await
                {
                    Ok(item) => child_task_items.push(item),
                    Err(RuntimeError::SnapshotStale) => continue 'snapshot,
                    Err(error) => return Err(error),
                }
            }
            if let Some((end, (approval_revision, mut summary, observed_runs))) = observation
                .capture(|| {
                    Ok((
                        self.approval_registry.revision(&request.session_id)?,
                        session.summary()?,
                        session.run_snapshots()?,
                    ))
                })?
                && approval_revision == approvals.revision
            {
                summary.pending_approval_count = approvals.items.len() as u64;
                summary.active_child_count = self
                    .child_tasks
                    .active_count_for_session(&request.session_id)?;
                let mut observed_runs: HashMap<_, _> = observed_runs
                    .into_iter()
                    .map(|run| (run.run_id.clone(), run))
                    .collect();
                for run in &mut runs {
                    if !run.status.is_terminal()
                        && let Some(observed) = observed_runs.remove(&run.run_id)
                    {
                        *run = observed;
                    }
                }
                let active_run = summary
                    .active_run_id
                    .as_ref()
                    .and_then(|id| runs.iter().find(|run| &run.run_id == id))
                    .cloned();
                return Ok(GetSessionViewResult {
                    snapshot: ObservedSnapshot {
                        observed_at_ms: super::now_ms().ok(),
                        observed_sequence: end,
                        value: SessionViewSnapshot {
                            session: summary,
                            agent_shell_kind,
                            title_generation,
                            workspace,
                            conversation_generation: generation,
                            composer_capabilities,
                            work_plan,
                            goal,
                            active_run,
                            queue,
                            approvals,
                            attachments,
                            file_references,
                            artifacts,
                            runs,
                            usage,
                            child_tasks: child_task_items,
                            active_skills,
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
        let _projection = crate::request_diagnostics::request_phase(
            crate::request_diagnostics::RequestPhase::Projection,
        );
        let session = self.session(&request.session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_session(
                &request.session_id,
                session.environment().workspace_id.clone(),
                SharedSnapshotSources::Configuration,
            );
            let stored = self
                .product_child_task(&request.session_id, &request.child_task_id)
                .await?
                .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                    session_id: request.session_id.clone(),
                    child_task_id: request.child_task_id.clone(),
                })?;
            let (approvals, approval_revision) = self
                .approval_registry
                .list_with_revision(&request.session_id)?;
            if stored.body_unavailable {
                return Err(RuntimeError::from_store(
                    "load inherited child body",
                    crate::StoreError::new(
                        StoreErrorKind::ResourceUnavailable,
                        "inherited child body is unavailable",
                    ),
                ));
            }
            let task = match self
                .child_task_tree_item(&request.session_id, stored.clone(), &approvals)
                .await
            {
                Ok(task) => task,
                Err(RuntimeError::SnapshotStale) => continue,
                Err(error) => return Err(error),
            };
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
                project_conversation_with_history(
                    &window.conversation,
                    window.history.as_ref(),
                    &projection,
                )?,
            )?;
            let approval_ids = approvals
                .iter()
                .filter(|approval| approval.child_task_id.as_ref() == Some(&request.child_task_id))
                .map(|approval| approval.approval_id.clone())
                .collect();
            if let Some((end, current_approval_revision)) =
                observation.capture(|| self.approval_registry.revision(&request.session_id))?
                && current_approval_revision == approval_revision
            {
                return Ok(GetChildTaskViewResult {
                    snapshot: ObservedSnapshot {
                        observed_at_ms: super::now_ms().ok(),
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
        let owner_session_id = match &request.owner {
            ConversationOwner::MainSession { session_id }
            | ConversationOwner::ChildTask { session_id, .. } => session_id,
        };
        let session = self.session(owner_session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        // 只保护当前 owner 的本地组合读取，不要求全局事件静止。
        let _mutation = session.mutation().await;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_history();
            let (generation, projection) = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    let attachments = self
                        .list_attachments(ListAttachmentsRequest {
                            session_id: session_id.clone(),
                        })
                        .await?
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
                        .product_child_task(session_id, child_task_id)
                        .await?
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
                project_conversation_with_history(
                    &window.conversation,
                    window.history.as_ref(),
                    &projection,
                )?,
            )?;
            if let ConversationOwner::ChildTask {
                session_id,
                child_task_id,
            } = &request.owner
            {
                let current = self
                    .product_child_task(session_id, child_task_id)
                    .await?
                    .ok_or_else(|| RuntimeError::ChildTaskNotFound {
                        session_id: session_id.clone(),
                        child_task_id: child_task_id.clone(),
                    })?;
                if current.body_generation != generation {
                    if request.cursor.is_none() {
                        continue;
                    }
                    return Err(RuntimeError::SnapshotStale);
                }
            }
            let Some((end, ())) = observation.capture(|| Ok(()))? else {
                continue;
            };
            return Ok(ListConversationPageResult {
                snapshot: ObservedSnapshot {
                    observed_at_ms: super::now_ms().ok(),
                    observed_sequence: end,
                    value: page,
                },
            });
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
        let session = self.session(&request.session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_history();
            let attachments = self
                .list_attachments(ListAttachmentsRequest {
                    session_id: request.session_id.clone(),
                })
                .await?
                .attachments;
            let history = self
                .store
                .load_conversation_history(ConversationOwner::MainSession {
                    session_id: request.session_id.clone(),
                })
                .await
                .map_err(|source| RuntimeError::from_store("load Run message history", source))?;
            let (generation, message_ids) = {
                let state = session.lock_state()?;
                if history.generation != state.body_generation {
                    continue;
                }
                let fact = history
                    .facts
                    .runs
                    .iter()
                    .find(|run| run.run_id == request.run_id);
                if fact.is_some_and(|run| run.inherited) {
                    return Err(RuntimeError::RunNotFound {
                        session_id: request.session_id.clone(),
                        run_id: request.run_id.clone(),
                    });
                }
                let mut ids: Vec<_> = history
                    .facts
                    .message_origins
                    .iter()
                    .filter(|origin| {
                        origin.run_id.as_ref() == Some(&request.run_id) && !origin.inherited
                    })
                    .map(|origin| origin.message_id.clone())
                    .collect();
                if ids.is_empty() {
                    if let Some(record) = state.runs.get(&request.run_id) {
                        ids = record.message_ids().to_vec();
                    } else if fact.is_none() {
                        return Err(RuntimeError::RunNotFound {
                            session_id: request.session_id.clone(),
                            run_id: request.run_id.clone(),
                        });
                    }
                }
                (state.body_generation, ids)
            };
            let owner = ConversationOwner::MainSession {
                session_id: request.session_id.clone(),
            };
            let mut anchor = None;
            for message_id in message_ids.iter().rev() {
                let location = self
                    .store
                    .locate_conversation_message(ConversationMessageLocationRequest {
                        owner: owner.clone(),
                        message_id: message_id.clone(),
                    })
                    .await
                    .map_err(|source| {
                        RuntimeError::from_store("locate run conversation message", source)
                    })?;
                if let Some(location) = location
                    && let Some(display_ordinal) = location.display_ordinal
                {
                    if location.generation != generation {
                        break;
                    }
                    anchor = Some((message_id.clone(), display_ordinal));
                    break;
                }
            }
            let Some((anchor_message_id, display_ordinal)) = anchor else {
                if session.lock_state()?.body_generation != generation {
                    continue;
                }
                return Err(RuntimeError::RunNotFound {
                    session_id: request.session_id.clone(),
                    run_id: request.run_id.clone(),
                });
            };
            let anchor_message_id = protocol_message_id(anchor_message_id.as_str())?;
            let requested_end = usize::try_from(display_ordinal)
                .ok()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "run conversation location",
                })?;
            let window = match self
                .load_product_conversation_window(
                    owner.clone(),
                    generation,
                    Some(requested_end),
                    limit,
                )
                .await
            {
                Ok(window) => window,
                Err(RuntimeError::SnapshotStale) => continue,
                Err(error) => return Err(error),
            };
            let page = page_from_window(
                owner,
                &window,
                project_conversation_with_history(
                    &window.conversation,
                    window.history.as_ref(),
                    &self.projection_context(&session, &attachments).await?,
                )?,
            )?;
            let Some((observed_sequence, ())) = observation.capture(|| Ok(()))? else {
                continue;
            };
            return Ok(GetConversationPageAroundRunResult {
                snapshot: ObservedSnapshot {
                    observed_at_ms: super::now_ms().ok(),
                    observed_sequence,
                    value: page,
                },
                anchor_message_id,
            });
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
        let caller = self.session(&request.session_id).await?.summary()?;
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
        let titles = self
            .store
            .search_conversation_titles(ConversationSearchRequest {
                query: query.to_owned(),
                scope: store_scope.clone(),
                limit: offset
                    .saturating_add(limit)
                    .saturating_add(1)
                    .min(MAX_PAGE_SIZE),
            })
            .await
            .map_err(|e| RuntimeError::from_store("search conversation titles", e))?;

        let mut items = titles.hits;
        let mut partial = titles.partial;
        let mut failed_owners = titles.failed_owners;
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
                .await;
            let page = match page {
                Ok(page) => page,
                // 标题事实仍可用时，FTS 不可用不应抹掉标题命中；返回既有 partial 提示。
                Err(source)
                    if source.kind() == crate::StoreErrorKind::Unavailable && !items.is_empty() =>
                {
                    crate::ConversationSearchPage {
                        hits: Vec::new(),
                        partial: true,
                        failed_owners: Vec::new(),
                    }
                }
                Err(source) => {
                    return Err(RuntimeError::from_store(
                        "search conversation history",
                        source,
                    ));
                }
            };
            partial |= page.partial;
            for owner in page.failed_owners {
                if !failed_owners.contains(&owner) {
                    failed_owners.push(owner);
                }
            }
            for hit in page.hits {
                let session_id = owner_session_id(&hit.owner);
                let session = self
                    .get_session(assistant_protocol::GetSessionRequest {
                        session_id: session_id.clone(),
                    })
                    .await?
                    .session;
                let child_task_title = hit.child_task_title;
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
        self.session(&request.session_id).await?;
        let before = usize::try_from(request.before.min(50)).expect("u32 fits usize");
        let after = usize::try_from(request.after.min(50)).expect("u32 fits usize");
        let (window, projection) = self
            .load_raw_window_around_message(&request.owner, &request.message_id, before, after)
            .await?;
        let items = project_conversation_with_history(
            &window.conversation,
            window.history.as_ref(),
            &projection,
        )?;
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
        let session_id = match &request.owner {
            ConversationOwner::MainSession { session_id }
            | ConversationOwner::ChildTask { session_id, .. } => session_id,
        };
        let session = self.session(session_id).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_history();
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
                project_conversation_with_history(
                    &window.conversation,
                    window.history.as_ref(),
                    &projection,
                )?,
            )?;
            let Some((observed_sequence, ())) = observation.capture(|| Ok(()))? else {
                continue;
            };
            return Ok(GetConversationPageAroundMessageResult {
                snapshot: ObservedSnapshot {
                    observed_at_ms: super::now_ms().ok(),
                    observed_sequence,
                    value: page,
                },
                anchor_message_id: request.message_id,
            });
        }
        Err(RuntimeError::SnapshotBusy)
    }

    async fn load_raw_window_around_message(
        &self,
        owner: &ConversationOwner,
        message_id: &MessageId,
        before: usize,
        after: usize,
    ) -> RuntimeResult<(crate::StoredConversationRawWindow, ProjectionContext)> {
        let session = self.session(owner_session_id(owner)).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        let _mutation = session.mutation().await;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let observation = self.event_sender.read_history();
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
            if observation.capture(|| Ok(()))?.is_some() {
                return Ok((window, projection));
            }
        }
        Err(RuntimeError::SnapshotBusy)
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
                let session = self.session(session_id).await?;
                let attachments = self
                    .list_attachments(ListAttachmentsRequest {
                        session_id: session_id.clone(),
                    })
                    .await?
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
        let session = self.session(owner_session_id(&request.owner)).await?;
        session
            .ensure_conversation_loaded(self.store.as_ref())
            .await?;
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let mutation = session.mutation().await;
            let observation = self.event_sender.read_history();
            let (snapshot, run_id) = match &request.owner {
                ConversationOwner::MainSession { session_id } => {
                    // 工具详情是当前 owner 的阅读查询；冷加载和继承历史不能借执行 Registry 找归属。
                    let history = self
                        .store
                        .load_conversation_history(request.owner.clone())
                        .await
                        .map_err(|source| {
                            RuntimeError::from_store("load tool detail history", source)
                        })?;
                    if history.generation != session.lock_state()?.body_generation {
                        continue;
                    }
                    let run_id = history
                        .facts
                        .message_origins
                        .iter()
                        .find(|origin| origin.message_id.as_str() == request.message_id.as_str())
                        .and_then(|origin| origin.run_id.clone());
                    let snapshot =
                        self.store
                            .load_conversation(session_id)
                            .await
                            .map_err(|source| {
                                RuntimeError::from_store("load tool detail conversation", source)
                            })?;
                    (snapshot, run_id)
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
            let mcp_identities = self.current_mcp_tool_identities()?;
            let mut detail = project_tool_detail(
                &snapshot,
                request.owner.clone(),
                &request.message_id,
                &request.call_id,
                run_id,
                &mcp_identities,
            )?;
            let Some((end, ())) = observation.capture(|| Ok(()))? else {
                continue;
            };
            // 工具正文基线已冻结。导航引用可以指向其他 Session，先释放 caller 屏障，
            // 避免 A 的工具详情等 B，而 B 又反等 A。目标导航仍按实际当前归属校验。
            drop(mutation);
            if detail.tool_name == "recall_memory" {
                detail.recall = self
                    .project_recall_tool_detail(&request.owner, detail.result_json.as_deref())
                    .await;
            }
            return Ok(GetToolDetailResult {
                snapshot: ObservedSnapshot {
                    observed_at_ms: super::now_ms().ok(),
                    observed_sequence: end,
                    value: detail,
                },
            });
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
        let caller_session = self.session(caller_session_id).await.ok()?;
        let recall = crate::conversation_recall::RuntimeConversationRecall::new(
            self.store.clone(),
            self.recall_reference_codec.clone(),
            caller_session_id.clone(),
            caller_session.environment().workspace_id.clone(),
        );
        let mut items = Vec::with_capacity(response.items.len());
        for item in response.items {
            let target = item
                .origins
                .iter()
                .filter_map(|origin| origin.reference.as_deref())
                .find_map(|reference| recall.resolve_reference(reference).ok());
            let navigation = if let Some(target) = target {
                let session_id = match &target.owner {
                    ConversationOwner::MainSession { session_id }
                    | ConversationOwner::ChildTask { session_id, .. } => session_id,
                };
                self.get_session(assistant_protocol::GetSessionRequest {
                    session_id: session_id.clone(),
                })
                .await
                .ok()
                .and_then(|summary| {
                    Some(RecallNavigationTarget {
                        owner: target.owner,
                        message_id: protocol_message_id(target.message_id.as_str()).ok()?,
                        lifecycle: summary.session.lifecycle,
                    })
                })
            } else {
                None
            };
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
        let session = self.session(session_id).await?;
        let (snapshot, environment) = match owner {
            ConversationOwner::MainSession { .. } => {
                session
                    .ensure_conversation_loaded(self.store.as_ref())
                    .await?;
                let snapshot =
                    self.store
                        .load_conversation(session_id)
                        .await
                        .map_err(|source| {
                            RuntimeError::from_store("load tool resource conversation", source)
                        })?;
                (snapshot, session.environment())
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
            let result = scoped_tool_results(&snapshot)
                .get(&(assistant.id.as_str(), call.id.as_str()))
                .copied();
            let Some(result) = result else {
                continue;
            };
            if call.name.as_str() == "edit_file"
                && let Some(entries) = result
                    .content
                    .as_single_json()
                    .and_then(|json| json.get("files"))
                    .and_then(serde_json::Value::as_array)
            {
                for (index, entry) in entries.iter().enumerate() {
                    if entry.get("status").and_then(serde_json::Value::as_str) != Some("success")
                        || tool_batch_resource_ref_id(&call.id, index).as_ref()
                            != Some(resource_ref_id)
                        || matches!(owner, ConversationOwner::ChildTask { .. })
                    {
                        continue;
                    }
                    let Some(path) = entry.get("path").and_then(serde_json::Value::as_str) else {
                        continue;
                    };
                    // 入口只从可靠成功位置派生；Error 整批仍可包含已成功文件。
                    let path = resolve_recorded_path(&environment.working_directory, path);
                    let path = self
                        .store
                        .current_recorded_resource_path(&path, environment);
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
            }
            if result.status != ToolResultStatus::Success {
                continue;
            }
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
                let path = self
                    .store
                    .current_recorded_resource_path(&path, environment);
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
        let (items, revision) = self.approval_registry.list_with_revision(session_id)?;
        let resolving_approval_id = items
            .iter()
            .find(|item| item.status == assistant_protocol::ApprovalStatus::Resolving)
            .map(|item| item.approval_id.clone());
        Ok(ApprovalQueueSnapshot {
            revision,
            items,
            resolving_approval_id,
        })
    }

    pub(super) async fn projection_context(
        &self,
        session: &crate::session::SessionController,
        attachments: &[assistant_protocol::AttachmentSummary],
    ) -> RuntimeResult<ProjectionContext> {
        let device_names = self.device_names()?;
        self.projection_context_with_devices(session, attachments, device_names)
            .await
    }

    async fn projection_context_with_devices(
        &self,
        session: &crate::session::SessionController,
        attachments: &[assistant_protocol::AttachmentSummary],
        device_names: HashMap<assistant_protocol::DeviceId, String>,
    ) -> RuntimeResult<ProjectionContext> {
        let (
            run_by_message,
            input_by_message,
            source_by_message,
            skill_by_message,
            mcp_selection_by_message,
            control_result_by_message,
        ) = {
            let state = session.lock_state()?;
            let mut run_by_message = HashMap::new();
            for run in state.runs.values() {
                let snapshot = run.snapshot();
                let input = state.inputs.get(run.input_id());
                let target = input.and_then(|input| input.stored.agent_shell_target);
                for message_id in run.message_ids() {
                    // 归属首次实际提交该消息的 attempt；启动前失败后的重试仍可首次提交。
                    if target.is_some()
                        && run.attempt() != 1
                        && input.is_some_and(|input| &input.stored.user_message_id == message_id)
                        && state.runs.values().any(|earlier| {
                            earlier.input_id() == run.input_id()
                                && earlier.attempt() < run.attempt()
                                && earlier.message_ids().contains(message_id)
                        })
                    {
                        continue;
                    }
                    run_by_message.insert(
                        message_id.as_str().to_owned(),
                        MessageRunProjection {
                            inherited: false,
                            partial: false,
                            shell_switch: target.map(|target| {
                                (
                                    target,
                                    run.shell().is_some_and(|shell| shell.kind == target),
                                )
                            }),
                            run_id: Some(snapshot.run_id.clone()),
                            attempt: Some(snapshot.attempt),
                            created_at_ms: snapshot.created_at_ms,
                            started_at_ms: snapshot.started_at_ms,
                            status: Some(snapshot.status),
                            error: snapshot.error.clone(),
                            finished_at_ms: run.finished_at_ms(),
                            step: run.message_step(message_id),
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
            let source_by_message = state
                .inputs
                .values()
                .map(|input| {
                    (
                        input.stored.user_message_id.as_str().to_owned(),
                        project_input_source(
                            input.stored.cross_session.as_ref(),
                            input.stored.channel_source.as_ref(),
                            &device_names,
                        ),
                    )
                })
                .collect();
            let skill_by_message = state
                .skill_activations
                .iter()
                .map(|activation| (activation.message_id.as_str().to_owned(), activation.tag()))
                .collect();
            let mcp_selection_by_message = state
                .mcp_selections
                .iter()
                .map(|selection| (selection.message_id.as_str().to_owned(), selection.tag()))
                .collect();
            let control_result_by_message = state
                .commands
                .values()
                .filter_map(|command| {
                    command
                        .result
                        .as_ref()
                        .map(|result| (command.user_message_id.as_str().to_owned(), result.clone()))
                })
                .collect();
            (
                run_by_message,
                input_by_message,
                source_by_message,
                skill_by_message,
                mcp_selection_by_message,
                control_result_by_message,
            )
        };
        let attachment_by_path = {
            let known =
                self.attachments
                    .read()
                    .map_err(|_| RuntimeError::InternalStateUnavailable {
                        component: "attachment registry",
                    })?;
            attachments
                .iter()
                .filter_map(|summary| known.get(&summary.attachment_id))
                .filter(|a| a.session_id == *session.id())
                .flat_map(|attachment| {
                    std::iter::once(attachment.agent_readable_path.clone())
                        .chain(self.store.historical_attachment_path(attachment))
                        .map(|path| (path, attachment.attachment_id.clone()))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let feedback_by_message = self
            .store
            .load_message_feedback(session.id())
            .await
            .map_err(|source| RuntimeError::from_store("load message feedback", source))?
            .into_iter()
            .map(|item| (item.message_id.as_str().to_owned(), item.feedback))
            .collect();
        Ok(ProjectionContext {
            device_names,
            run_by_message,
            input_by_message,
            source_by_message,
            skill_by_message,
            mcp_selection_by_message,
            control_result_by_message,
            attachment_by_path,
            feedback_by_message,
            mcp_identities: self.current_mcp_tool_identities()?,
            can_fork: true,
        })
    }

    pub(super) fn current_mcp_tool_identities(
        &self,
    ) -> RuntimeResult<HashMap<(String, String), McpToolIdentity>> {
        Ok(self
            .mcp_service
            .registry
            .tool_identities()?
            .into_iter()
            .map(|identity| {
                (
                    (
                        identity.server_key.as_str().to_owned(),
                        identity.tool_name.clone(),
                    ),
                    identity,
                )
            })
            .collect())
    }

    async fn child_usage_snapshot(
        &self,
        session_id: &SessionId,
        task: &crate::StoredChildTaskHistory,
    ) -> RuntimeResult<ChildTaskUsageSnapshot> {
        let usage = self
            .store
            .get_conversation_usage(ConversationOwner::ChildTask {
                session_id: session_id.clone(),
                child_task_id: task.child_task_id.clone(),
            })
            .await
            .map_err(|source| RuntimeError::from_store("load child usage snapshot", source))?;
        let mut context = project_context_usage(
            usage.used_tokens(),
            task.context_window_tokens,
            task.context_management_mode,
        );
        if let Some(context) = &mut context {
            context.calibration_pending = usage.calibration_pending.then_some(true);
        }
        Ok(ChildTaskUsageSnapshot {
            context,
            accumulated: usage.accumulated.as_ref().map(usage_totals),
        })
    }

    /// 主会话列表读取 child 目录和 SQL 用量快照；最终回复正文在打开该 child 时读取。
    async fn child_task_tree_summary(
        &self,
        session_id: &SessionId,
        mut task: crate::StoredChildTaskHistory,
        approvals: &[assistant_protocol::ApprovalSnapshot],
    ) -> RuntimeResult<ChildTaskTreeItemSnapshot> {
        let usage = self.child_usage_snapshot(session_id, &task).await?;
        task.final_message_id = None;
        let snapshot = self.child_history_snapshot(session_id, task).await?;
        let pending_approval_count = approvals
            .iter()
            .filter(|approval| approval.child_task_id.as_ref() == Some(&snapshot.child_task_id))
            .count();
        Ok(ChildTaskTreeItemSnapshot {
            can_cancel: !snapshot.inherited && !snapshot.status.is_terminal(),
            task: snapshot,
            usage,
            pending_approval_count: pending_approval_count as u64,
        })
    }

    /// 调用方已在同一 Session 观察屏障内核验主文件目录并取得 child 阅读事实。
    /// 本函数复用该事实，正文／最终消息读取仍核验 generation；完成前由调用方复核
    /// Session 水位（含 child 提交事件），避免每个 child 再完整读取两次主历史。
    async fn child_task_tree_item(
        &self,
        session_id: &SessionId,
        task: crate::StoredChildTaskHistory,
        approvals: &[assistant_protocol::ApprovalSnapshot],
    ) -> RuntimeResult<ChildTaskTreeItemSnapshot> {
        let current = task.clone();
        if current.inherited && current.body_unavailable {
            return Ok(ChildTaskTreeItemSnapshot {
                task: self.child_history_snapshot(session_id, current).await?,
                usage: ChildTaskUsageSnapshot {
                    context: None,
                    accumulated: None,
                },
                pending_approval_count: 0,
                can_cancel: false,
            });
        }
        let usage = self.child_usage_snapshot(session_id, &current).await?;
        let snapshot = self.child_history_snapshot(session_id, current).await?;
        let pending_approval_count = approvals
            .iter()
            .filter(|approval| approval.child_task_id.as_ref() == Some(&snapshot.child_task_id))
            .count();
        Ok(ChildTaskTreeItemSnapshot {
            can_cancel: !snapshot.inherited && !snapshot.status.is_terminal(),
            task: snapshot,
            usage,
            pending_approval_count: u64::try_from(pending_approval_count).unwrap_or(u64::MAX),
        })
    }
}

impl AssistantRuntime {
    async fn composer_capabilities(
        &self,
        snapshot: &crate::config::ConfigSnapshot,
        model_selection: Option<&assistant_protocol::ModelSelection>,
    ) -> RuntimeResult<(ComposerCapabilitiesSnapshot, Option<u64>)> {
        if let Err(error) = self.model_factory.ensure_available() {
            return Ok((
                unavailable_composer_capabilities(None, Some(error.to_protocol_info())),
                None,
            ));
        }
        let prepared = match self
            .config_registry
            .prepare_model(snapshot, model_selection, self.store.as_ref())
            .await
        {
            Ok(prepared) => prepared,
            Err(RuntimeError::ConfigurationConflict) => return Err(RuntimeError::SnapshotStale),
            Err(error) => {
                let selected = self
                    .config_registry
                    .effective_model_selection(model_selection)?;
                return Ok((
                    unavailable_composer_capabilities(selected, Some(error.to_protocol_info())),
                    None,
                ));
            }
        };
        let model = &prepared.model;
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
        } else if model.capabilities().tool_calls
            && self
                .config_registry
                .managed_models()?
                .settings
                .vision_model
                .is_some()
        {
            ImageHandlingMode::Tool
        } else {
            ImageHandlingMode::Unavailable
        };
        prepared
            .ensure_current(&self.config_registry)
            .map_err(|error| match error {
                RuntimeError::ConfigurationConflict => RuntimeError::SnapshotStale,
                error => error,
            })?;
        Ok((
            ComposerCapabilitiesSnapshot {
                selected_model: Some(prepared.selection.clone()),
                model_error: None,
                reasoning_effort_options,
                image_handling,
                goal_supported: model.capabilities().tool_calls,
            },
            Some(model.context_window_tokens()),
        ))
    }
}

fn unavailable_composer_capabilities(
    selected_model: Option<assistant_protocol::ModelSelection>,
    model_error: Option<assistant_protocol::RuntimeErrorInfo>,
) -> ComposerCapabilitiesSnapshot {
    ComposerCapabilitiesSnapshot {
        selected_model,
        model_error,
        reasoning_effort_options: Vec::new(),
        image_handling: ImageHandlingMode::Unavailable,
        goal_supported: false,
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

pub(super) fn project_skill_management(
    discovery: &crate::SkillDiscovery,
    states: &[crate::SkillNameState],
    include_plugins: bool,
) -> SkillManagementSnapshot {
    let winners_by_name = discovery
        .winners
        .iter()
        .map(|candidate| (candidate.name.clone(), candidate))
        .collect::<std::collections::BTreeMap<_, _>>();
    let conflict_names = discovery
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic.code,
                crate::SkillDiagnosticCode::SameSourceConflict
                    | crate::SkillDiagnosticCode::PluginConflict
            )
        })
        .filter_map(|diagnostic| diagnostic.skill_name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut by_name = std::collections::BTreeMap::new();
    for candidate in &discovery.candidates {
        if candidate.source == crate::SkillSource::Plugin {
            continue;
        }
        by_name.entry(candidate.name.clone()).or_insert(candidate);
    }
    if include_plugins {
        for candidate in &discovery.candidates {
            by_name.entry(candidate.name.clone()).or_insert(candidate);
        }
        for winner in &discovery.winners {
            if winner.source == crate::SkillSource::Plugin {
                by_name.insert(winner.name.clone(), winner);
            }
        }
    }
    let skills = by_name
        .into_iter()
        .map(|(name, candidate)| {
            // 插件整体启用由扫描入口决定，不继承同名独立技能的历史开关。
            let enabled = crate::SkillNameState::enabled_for(
                name.as_str(),
                candidate.plugin_name.as_deref(),
                states,
            );
            let health = if !enabled {
                SkillHealthSnapshot::Disabled
            } else if discovery.status == crate::SkillDiscoveryStatus::Unavailable {
                SkillHealthSnapshot::Unavailable
            } else if conflict_names.contains(&name) {
                SkillHealthSnapshot::Conflict
            } else if winners_by_name
                .get(&name)
                .is_some_and(|winner| *winner == candidate)
            {
                SkillHealthSnapshot::Ready
            } else {
                SkillHealthSnapshot::Unavailable
            };
            SkillSummarySnapshot {
                name: name.as_str().to_owned(),
                plugin_name: candidate.plugin_name.clone(),
                description: candidate.description.clone(),
                source: project_skill_source(candidate.source),
                model_invocable: candidate.model_invocable,
                user_invocable: candidate.user_invocable,
                enabled,
                health,
            }
        })
        .collect();
    SkillManagementSnapshot {
        available: discovery.status == crate::SkillDiscoveryStatus::Available,
        skills,
        diagnostics: discovery
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                include_plugins || diagnostic.source != Some(crate::SkillSource::Plugin)
            })
            .map(project_skill_diagnostic)
            .collect(),
    }
}

fn project_active_skills(
    state: &crate::session::SessionState,
) -> RuntimeResult<Vec<ActiveSkillSnapshot>> {
    let mut latest = std::collections::BTreeMap::new();
    for activation in &state.skill_activations {
        if !matches!(&activation.owner, crate::SkillActivationOwner::Session(_)) {
            continue;
        }
        let is_committed = activation.input_id.as_ref().is_none_or(|input_id| {
            state
                .inputs
                .get(input_id)
                .is_some_and(|input| input.stored.state == crate::StoredInputState::Committed)
        });
        if is_committed {
            latest.insert(activation.name.clone(), activation);
        }
    }
    let mut active = latest
        .into_values()
        .map(|activation| {
            Ok(ActiveSkillSnapshot {
                tag: activation.tag(),
                trigger: match activation.trigger {
                    crate::SkillActivationTrigger::User => SkillActivationTriggerSnapshot::User,
                    crate::SkillActivationTrigger::Model => SkillActivationTriggerSnapshot::Model,
                },
                message_id: protocol_message_id(activation.message_id.as_str())?,
                created_at_ms: activation.created_at_ms,
            })
        })
        .collect::<RuntimeResult<Vec<_>>>()?;
    active.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then(left.message_id.as_str().cmp(right.message_id.as_str()))
    });
    Ok(active)
}

fn project_skill_source(source: crate::SkillSource) -> SkillSourceSnapshot {
    match source {
        crate::SkillSource::WorkspaceEzAssistant => SkillSourceSnapshot::WorkspaceEzAssistant,
        crate::SkillSource::WorkspaceAgents => SkillSourceSnapshot::WorkspaceAgents,
        crate::SkillSource::UserEzAssistant => SkillSourceSnapshot::UserEzAssistant,
        crate::SkillSource::LegacyUserEzAssistant => SkillSourceSnapshot::LegacyUserEzAssistant,
        crate::SkillSource::HostSupplement => SkillSourceSnapshot::HostSupplement,
        crate::SkillSource::UserAgents => SkillSourceSnapshot::UserAgents,
        crate::SkillSource::Plugin => SkillSourceSnapshot::Plugin,
        crate::SkillSource::Builtin => SkillSourceSnapshot::Builtin,
    }
}

fn project_skill_diagnostic(diagnostic: &crate::SkillDiagnostic) -> SkillDiagnosticSnapshot {
    SkillDiagnosticSnapshot {
        severity: match diagnostic.severity {
            crate::SkillDiagnosticSeverity::Warning => SkillDiagnosticSeveritySnapshot::Warning,
            crate::SkillDiagnosticSeverity::Error => SkillDiagnosticSeveritySnapshot::Error,
        },
        code: skill_diagnostic_code(diagnostic.code).to_owned(),
        skill_name: diagnostic
            .skill_name
            .as_ref()
            .map(|name| name.as_str().to_owned()),
        source: diagnostic.source.map(project_skill_source),
        detail: diagnostic.detail.clone(),
    }
}

fn skill_diagnostic_code(code: crate::SkillDiagnosticCode) -> &'static str {
    match code {
        crate::SkillDiagnosticCode::RootUnreadable => "root_unreadable",
        crate::SkillDiagnosticCode::ScanIncomplete => "scan_incomplete",
        crate::SkillDiagnosticCode::CandidateLimitExceeded => "candidate_limit_exceeded",
        crate::SkillDiagnosticCode::MissingDefinition => "missing_definition",
        crate::SkillDiagnosticCode::DefinitionTooLarge => "definition_too_large",
        crate::SkillDiagnosticCode::FrontmatterTooLarge => "frontmatter_too_large",
        crate::SkillDiagnosticCode::InvalidFrontmatter => "invalid_frontmatter",
        crate::SkillDiagnosticCode::MissingRequiredField => "missing_required_field",
        crate::SkillDiagnosticCode::InvalidName => "invalid_name",
        crate::SkillDiagnosticCode::InvalidDescription => "invalid_description",
        crate::SkillDiagnosticCode::OptionalFieldDefaulted => "optional_field_defaulted",
        crate::SkillDiagnosticCode::UnknownField => "unknown_field",
        crate::SkillDiagnosticCode::SpecialFile => "special_file",
        crate::SkillDiagnosticCode::SameSourceConflict => "same_source_conflict",
        crate::SkillDiagnosticCode::DisabledByName => "disabled_by_name",
        crate::SkillDiagnosticCode::Shadowed => "shadowed",
        crate::SkillDiagnosticCode::PluginConflict => "plugin_conflict",
        crate::SkillDiagnosticCode::NotInvocable => "not_invocable",
        crate::SkillDiagnosticCode::CatalogLimitExceeded => "catalog_limit_exceeded",
        crate::SkillDiagnosticCode::LegacyCatalogUnavailable => "legacy_catalog_unavailable",
    }
}

pub(super) fn empty_child_projection() -> ProjectionContext {
    ProjectionContext {
        device_names: HashMap::new(),
        run_by_message: HashMap::new(),
        input_by_message: HashMap::new(),
        source_by_message: HashMap::new(),
        skill_by_message: HashMap::new(),
        mcp_selection_by_message: HashMap::new(),
        control_result_by_message: HashMap::new(),
        attachment_by_path: HashMap::new(),
        feedback_by_message: HashMap::new(),
        mcp_identities: HashMap::new(),
        can_fork: false,
    }
}

pub(super) fn queue_snapshot(
    session: &crate::session::SessionController,
    device_names: &HashMap<assistant_protocol::DeviceId, String>,
) -> RuntimeResult<QueueSnapshot> {
    let state = session.lock_state()?;
    let held_by_goal = state.goal.is_some();
    let items = state
        .queue_item_ids
        .iter()
        .enumerate()
        .filter_map(|(position, input_id)| {
            let position = u32::try_from(position).unwrap_or(u32::MAX);
            if let Some(input) = state.inputs.get(input_id) {
                let text_preview = input
                    .stored
                    .queued_message
                    .as_ref()
                    .map(user_text)
                    .unwrap_or_default();
                return Some(QueuedSessionItemSnapshot::Message(QueuedInputSnapshot {
                    input_id: input_id.clone(),
                    agent_shell_target: input.stored.agent_shell_target,
                    text_preview: truncate_chars(&text_preview, TOOL_SUMMARY_CHARS),
                    submitted_at_ms: input.stored.accepted_at_ms,
                    position,
                    is_prioritized: position == 0,
                    source: project_input_source(
                        input.stored.cross_session.as_ref(),
                        input.stored.channel_source.as_ref(),
                        device_names,
                    ),
                    held_by_goal,
                    skill: input
                        .stored
                        .skill_activation
                        .as_ref()
                        .map(crate::StoredSkillActivation::tag),
                    mcp_selection: state
                        .mcp_selections
                        .iter()
                        .find(|selection| selection.input_id.as_ref() == Some(input_id))
                        .map(crate::StoredMcpSelection::tag),
                }));
            }
            state.commands.get(input_id).map(|command| {
                QueuedSessionItemSnapshot::Command(QueuedSessionCommandSnapshot {
                    input_id: input_id.clone(),
                    command: command.command.clone(),
                    state: if state.executing_command.as_ref() == Some(input_id) {
                        assistant_protocol::SessionCommandQueueState::Executing
                    } else {
                        assistant_protocol::SessionCommandQueueState::Queued
                    },
                    submitted_at_ms: command.accepted_at_ms,
                    position,
                    is_prioritized: position == 0,
                })
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

pub(super) fn project_work_plan(plan: &crate::work_plan::WorkPlan) -> WorkPlanSnapshot {
    WorkPlanSnapshot {
        revision: plan.revision,
        objective: plan.objective.clone(),
        items: plan
            .items
            .iter()
            .map(|item| WorkPlanItemSnapshot {
                id: item.id.clone(),
                text: item.text.clone(),
                status: match item.status {
                    crate::work_plan::TodoItemStatus::Pending => TodoItemStatusSnapshot::Pending,
                    crate::work_plan::TodoItemStatus::InProgress => {
                        TodoItemStatusSnapshot::InProgress
                    }
                    crate::work_plan::TodoItemStatus::Completed => {
                        TodoItemStatusSnapshot::Completed
                    }
                },
            })
            .collect(),
        updated_at_ms: plan.updated_at_ms,
    }
}

pub(crate) fn project_goal(goal: &crate::goal::GoalControl) -> RuntimeResult<GoalSnapshot> {
    let mut objective_text = String::new();
    let mut attachment_count = 0usize;
    for part in &goal.objective.payload {
        match part {
            crate::goal::GoalObjectivePart::Text(part) => {
                if !objective_text.is_empty() {
                    objective_text.push('\n');
                }
                objective_text.push_str(&part.text);
            }
            crate::goal::GoalObjectivePart::FileReferences(part) => {
                attachment_count = attachment_count.saturating_add(part.files.len());
            }
        }
    }
    let (state, pause_reason) = match &goal.state {
        crate::goal::GoalState::Running => (GoalStateSnapshot::Running, None),
        crate::goal::GoalState::Paused(reason) => (
            GoalStateSnapshot::Paused,
            Some(match reason {
                crate::goal::GoalPauseReason::Blocked { summary } => {
                    GoalPauseReasonSnapshot::Blocked {
                        summary: summary.clone(),
                    }
                }
                crate::goal::GoalPauseReason::UserStopped => GoalPauseReasonSnapshot::UserStopped,
                crate::goal::GoalPauseReason::RunLimitReached => {
                    GoalPauseReasonSnapshot::RunLimitReached
                }
                crate::goal::GoalPauseReason::ConsecutiveFailures => {
                    GoalPauseReasonSnapshot::ConsecutiveFailures
                }
                crate::goal::GoalPauseReason::RecoveryRequired => {
                    GoalPauseReasonSnapshot::RecoveryRequired
                }
                crate::goal::GoalPauseReason::Forked => GoalPauseReasonSnapshot::Forked,
            }),
        ),
        crate::goal::GoalState::Completed => (GoalStateSnapshot::Completed, None),
    };
    Ok(GoalSnapshot {
        goal_id: goal.id.clone(),
        objective_message_id: protocol_message_id(goal.objective.source_message_id.as_str())?,
        objective_preview: truncate_chars(&objective_text, TOOL_SUMMARY_CHARS),
        attachment_count: u32::try_from(attachment_count).unwrap_or(u32::MAX),
        mcp_server_key: goal.mcp_server_key.clone(),
        state,
        pause_reason,
        generation: goal.generation,
        turn: goal.turn,
        budget: GoalBudgetSnapshot {
            max_runs: goal.budget.max_runs,
            max_consecutive_failures: goal.budget.max_consecutive_failures,
            used_runs: goal.budget.used_runs,
            used_total_tokens: goal.budget.used_total_tokens,
            consecutive_failures: goal.consecutive_failures,
            usage_complete: goal.budget.usage_complete,
        },
        created_at_ms: goal.created_at_ms,
        updated_at_ms: goal.updated_at_ms,
        completed_at_ms: goal.completed_at_ms,
    })
}

/// 工具 ID 只在各压缩边界内唯一；产品详情还必须以原 Assistant 消息定位结果。
fn scoped_tool_results(
    snapshot: &ConversationSnapshot,
) -> HashMap<(&str, &str), &agent_types::ToolResult> {
    let mut results = HashMap::new();
    let mut assistant = None;
    for message in &snapshot.messages {
        match message {
            ConversationMessage::Assistant(message) => assistant = Some(message.id.as_str()),
            ConversationMessage::Tool(message) => {
                if let Some(assistant) = assistant {
                    results.insert(
                        (assistant, message.result.call_id.as_str()),
                        &message.result,
                    );
                }
            }
            _ => assistant = None,
        }
    }
    results
}

/// 将可靠 Assistant/Tool 及其文件归属投影为 Run 工具列表；不恢复任何执行或流式输出。
/// 输入已由调用方按 owner/generation 读取，本函数不取锁、不访问 Store，可重复投影。
/// 结果以原 Assistant 和 call ID 配对，避免不同 Step 复用 call ID 时串用结果。
pub(super) fn project_run_tool_activities(
    snapshot: &ConversationSnapshot,
    origins: &[crate::StoredMessageOrigin],
    identities: &HashMap<(String, String), McpToolIdentity>,
) -> RuntimeResult<HashMap<RunId, Vec<assistant_protocol::ToolActivitySnapshot>>> {
    let origins: HashMap<_, _> = origins
        .iter()
        .map(|origin| (&origin.message_id, origin))
        .collect();
    let results = scoped_tool_results(snapshot);
    let mut tools = HashMap::<_, Vec<_>>::new();
    for message in &snapshot.messages {
        let ConversationMessage::Assistant(assistant) = message else {
            continue;
        };
        let Some(origin) = origins.get(&assistant.id) else {
            continue;
        };
        let Some(run_id) = &origin.run_id else {
            continue;
        };
        for part in &assistant.parts {
            let AssistantPart::ToolCall(call) = part else {
                continue;
            };
            let identity = project_mcp_identity(call.name.as_str(), &call.arguments, identities);
            tools.entry(run_id.clone()).or_default().push(
                assistant_protocol::ToolActivitySnapshot {
                    step: origin.step,
                    call_id: protocol_tool_call_id(call.id.as_str())?,
                    tool_name: call.name.as_str().to_owned(),
                    input: project_tool_input(
                        call.name.as_str(),
                        &call.arguments,
                        identity.as_ref(),
                    ),
                    mcp_identity: identity,
                    status: tool_status(
                        results
                            .get(&(assistant.id.as_str(), call.id.as_str()))
                            .copied(),
                    ),
                    stdout: String::new(),
                    stderr: String::new(),
                },
            );
        }
    }
    Ok(tools)
}

/// 文件窗口中的历史值只参与展示，不创建或恢复 RunRecord。
pub(super) fn project_conversation_with_history(
    snapshot: &ConversationSnapshot,
    history: Option<&crate::StoredHistoryFacts>,
    context: &ProjectionContext,
) -> RuntimeResult<Vec<ConversationItem>> {
    let Some(history) = history else {
        return project_conversation(snapshot, context);
    };
    let mut projection = context.clone();
    let inputs: HashMap<_, _> = history
        .inputs
        .iter()
        .map(|input| (&input.input_id, input))
        .collect();
    for input in &history.inputs {
        projection.input_by_message.insert(
            input.user_message_id.as_str().to_owned(),
            input.input_id.clone(),
        );
        projection.source_by_message.insert(
            input.user_message_id.as_str().to_owned(),
            project_input_source(
                input.cross_session.as_ref(),
                input.channel_source.as_ref(),
                &context.device_names,
            ),
        );
    }
    let runs: HashMap<_, _> = history.runs.iter().map(|run| (&run.run_id, run)).collect();
    for origin in &history.message_origins {
        let run = origin.run_id.as_ref().and_then(|id| runs.get(id)).copied();
        let existing = context.run_by_message.get(origin.message_id.as_str());
        let input = origin
            .input_id
            .as_ref()
            .or_else(|| run.and_then(|run| run.input_id.as_ref()))
            .and_then(|id| inputs.get(id).copied());
        let shell_switch = input
            .and_then(|input| input.agent_shell_target)
            .map(|target| {
                (
                    target,
                    run.and_then(|run| run.agent_shell_kind) == Some(target),
                )
            })
            .or_else(|| existing.and_then(|record| record.shell_switch));
        let mut record = MessageRunProjection {
            inherited: origin.inherited || run.is_some_and(|run| run.inherited),
            partial: run.is_some_and(|run| run.partial),
            shell_switch,
            run_id: origin.run_id.clone(),
            attempt: origin.attempt,
            created_at_ms: run.map(|run| run.created_at_ms),
            started_at_ms: run.and_then(|run| run.started_at_ms),
            status: run.and_then(|run| run.status),
            error: run.and_then(|run| run.error.clone()),
            finished_at_ms: run.and_then(|run| run.finished_at_ms),
            step: origin.step,
        };
        // 执行 Registry 只覆盖当前实际执行的实时值；文件历史本身不安装为执行对象。
        if let Some(existing) = existing
            && existing.run_id == record.run_id
            && !origin.inherited
            && !run.is_some_and(|run| run.inherited || run.partial)
            && !record.status.is_some_and(|status| status.is_terminal())
            && existing.status.is_some_and(|status| !status.is_terminal())
        {
            record.status = existing.status;
            record.finished_at_ms = existing.finished_at_ms;
            record.error = existing.error.clone();
        }
        projection
            .run_by_message
            .insert(origin.message_id.as_str().to_owned(), record);
        if let Some(input) = &origin.input_id {
            projection
                .input_by_message
                .insert(origin.message_id.as_str().to_owned(), input.clone());
        }
    }
    for command in &history.commands {
        if let Some(result) = &command.result {
            projection
                .control_result_by_message
                .insert(command.user_message_id.as_str().to_owned(), result.clone());
        }
    }
    for activation in &history.skill_activations {
        projection
            .skill_by_message
            .insert(activation.message_id.as_str().to_owned(), activation.tag());
    }
    for selection in &history.mcp_selections {
        projection
            .mcp_selection_by_message
            .insert(selection.message_id.as_str().to_owned(), selection.tag());
    }
    for feedback in &history.feedback {
        if let Some(value) = feedback.feedback {
            projection
                .feedback_by_message
                .insert(feedback.message_id.as_str().to_owned(), value);
        } else {
            projection
                .feedback_by_message
                .remove(feedback.message_id.as_str());
        }
    }
    project_conversation(snapshot, &projection)
}

pub(super) fn project_conversation(
    snapshot: &ConversationSnapshot,
    context: &ProjectionContext,
) -> RuntimeResult<Vec<ConversationItem>> {
    let tool_results = scoped_tool_results(snapshot);
    let mut items = Vec::new();
    for message in &snapshot.messages {
        match message {
            ConversationMessage::User(user) if user.transcript_visibility.is_visible() => {
                if let Some(result) = crate::shell::InheritedShellSwitchResult::from_message(user)?
                {
                    items.push(ConversationItem::ShellSwitchResult {
                        message_id: protocol_message_id(user.id.as_str())?,
                        previous_shell: crate::shell::switch_previous(user)?,
                        run_id: None,
                        shell: result.shell,
                        success: result.success,
                        error: result.error,
                    });
                    continue;
                }
                if let Some(run) = context.run_by_message.get(user.id.as_str())
                    && let Some((shell, success)) = run.shell_switch
                {
                    items.push(ConversationItem::ShellSwitchResult {
                        message_id: protocol_message_id(user.id.as_str())?,
                        previous_shell: crate::shell::switch_previous(user)?,
                        run_id: run.run_id.clone(),
                        shell,
                        success,
                        error: if success { None } else { run.error.clone() },
                    });
                    continue;
                }
                if let Some(result) = context.control_result_by_message.get(user.id.as_str()) {
                    let message_id = protocol_message_id(user.id.as_str())?;
                    items.push(match result {
                        crate::StoredSessionCommandResult::ShellSwitch {
                            shell,
                            previous_shell,
                            environment,
                            error,
                        } => ConversationItem::ShellSwitchResult {
                            message_id,
                            previous_shell: *previous_shell,
                            run_id: None,
                            shell: *shell,
                            success: environment.is_some(),
                            error: error.clone(),
                        },
                        crate::StoredSessionCommandResult::Mcp(result) => {
                            ConversationItem::ControlResult {
                                message_id,
                                result: result.clone(),
                            }
                        }
                        crate::StoredSessionCommandResult::SkillRefresh {
                            success,
                            skill_count,
                        } => ConversationItem::SkillRefreshResult {
                            message_id,
                            success: *success,
                            skill_count: *skill_count,
                        },
                    });
                    continue;
                }
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
                let quotes = user
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        UserPart::QuotedText(quoted) => {
                            Some(assistant_protocol::QuotedTextSnapshot {
                                quote_id: protocol_part_id(quoted.quote_id.as_str()).ok()?,
                                exact: quoted.exact.clone(),
                                prefix: quoted.prefix.clone(),
                                suffix: quoted.suffix.clone(),
                                source_owner: match &quoted.source_owner {
                                    agent_types::QuotedTextSourceOwner::MainSession {
                                        session_id,
                                    } => assistant_protocol::ConversationOwner::MainSession {
                                        session_id: assistant_protocol::SessionId::new(
                                            session_id.clone(),
                                        )
                                        .ok()?,
                                    },
                                    agent_types::QuotedTextSourceOwner::ChildTask {
                                        session_id,
                                        child_task_id,
                                    } => assistant_protocol::ConversationOwner::ChildTask {
                                        session_id: assistant_protocol::SessionId::new(
                                            session_id.clone(),
                                        )
                                        .ok()?,
                                        child_task_id: assistant_protocol::ChildTaskId::new(
                                            child_task_id.clone(),
                                        )
                                        .ok()?,
                                    },
                                },
                                source_generation: quoted.source_generation,
                                source_message_id: protocol_message_id(
                                    quoted.source_message_id.as_str(),
                                )
                                .ok()?,
                                text_start_utf16: quoted.text_start_utf16,
                                text_end_utf16: quoted.text_end_utf16,
                                source_role: match quoted.source_role {
                                    agent_types::QuotedTextSourceRole::User => {
                                        assistant_protocol::QuotedTextSourceRoleSnapshot::User
                                    }
                                    agent_types::QuotedTextSourceRole::Assistant => {
                                        assistant_protocol::QuotedTextSourceRoleSnapshot::Assistant
                                    }
                                },
                                source_label: quoted.source_label.clone(),
                                source_created_at_ms: quoted.source_created_at_ms,
                                source_available: quoted.source_available,
                            })
                        }
                        _ => None,
                    })
                    .collect();
                items.push(ConversationItem::User(UserMessageSnapshot {
                    inherited: context
                        .run_by_message
                        .get(user.id.as_str())
                        .is_some_and(|origin| origin.inherited),
                    message_id: protocol_message_id(user.id.as_str())?,
                    input_id: context.input_by_message.get(user.id.as_str()).cloned(),
                    text: user_text(user),
                    attachment_ids,
                    quotes,
                    source: context
                        .source_by_message
                        .get(user.id.as_str())
                        .cloned()
                        .unwrap_or_default(),
                    skill: context.skill_by_message.get(user.id.as_str()).cloned(),
                    mcp_selection: context
                        .mcp_selection_by_message
                        .get(user.id.as_str())
                        .cloned(),
                    created_at_ms: None,
                }));
            }
            ConversationMessage::Assistant(assistant) if assistant.purpose.is_model_response() => {
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
                                source: crate::run::project_reasoning_source(part.source),
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
                            let result = tool_results
                                .get(&(assistant.id.as_str(), call.id.as_str()))
                                .copied();
                            let mcp_identity = project_mcp_identity(
                                call.name.as_str(),
                                &call.arguments,
                                &context.mcp_identities,
                            );
                            pending_tools.push(ToolEventSnapshot {
                                call_id: protocol_tool_call_id(call.id.as_str())?,
                                tool_name: call.name.as_str().to_owned(),
                                mcp_identity: mcp_identity.clone(),
                                status: tool_status(result),
                                summary: result.map(tool_result_summary),
                                input: project_tool_input(
                                    call.name.as_str(),
                                    &call.arguments,
                                    mcp_identity.as_ref(),
                                ),
                            });
                        }
                        AssistantPart::ProviderState(_) => {}
                    }
                }
                flush_tools(&mut segments, &mut pending_tools);
                let run = context.run_by_message.get(assistant.id.as_str());
                items.push(ConversationItem::Assistant(AssistantMessageSnapshot {
                    inherited: run.is_some_and(|run| run.inherited),
                    partial: run.is_some_and(|run| run.partial),
                    run_created_at_ms: run.and_then(|run| run.created_at_ms),
                    run_started_at_ms: run.and_then(|run| run.started_at_ms),
                    message_id: protocol_message_id(assistant.id.as_str())?,
                    run_id: run.and_then(|run| run.run_id.clone()),
                    attempt: run.and_then(|run| run.attempt),
                    step: run.and_then(|run| run.step),
                    created_at_ms: None,
                    finished_at_ms: run.and_then(|run| run.finished_at_ms),
                    status: run.and_then(|run| run.status),
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
            ConversationMessage::ContextSummary(summary) => {
                items.push(ConversationItem::ContextSummary {
                    message_id: protocol_message_id(summary.id.as_str())?,
                    text: summary.text.clone(),
                });
            }
            ConversationMessage::User(_)
            | ConversationMessage::System(_)
            | ConversationMessage::Assistant(_)
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
    mcp_identities: &HashMap<(String, String), McpToolIdentity>,
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
    let result = scoped_tool_results(snapshot)
        .get(&(message_id.as_str(), call_id.as_str()))
        .copied();
    let summary = result.map(tool_result_summary);
    let mcp_identity = project_mcp_identity(call.name.as_str(), &call.arguments, mcp_identities);
    let mut input = project_tool_input(call.name.as_str(), &call.arguments, mcp_identity.as_ref());
    let files = project_tool_files(
        &call.id,
        &input.value,
        result,
        workspace_resources_available,
    )?;
    let safe_management = (call.name.as_str() == "configure_mcp")
        .then(|| super::mcp::management_tools::safe_arguments(&call.arguments));
    let (request_json, request_redacted, request_truncated) =
        project_json(safe_management.as_ref().unwrap_or(&call.arguments));
    input.redacted |= request_redacted;
    input.truncated |= request_truncated;
    let (result_json, result_truncated) =
        result
            .and_then(tool_result_json)
            .map_or((None, false), |value| {
                let (formatted, truncated) = formatted_json(&value);
                (Some(formatted), truncated)
            });
    let output_truncated = input.truncated || result_truncated;
    Ok(ToolDetailSnapshot {
        owner,
        message_id: message_id.clone(),
        run_id,
        call_id: call_id.clone(),
        tool_name: call.name.as_str().to_owned(),
        mcp_identity,
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
        output_truncated,
        historical_fields_missing: result.is_none(),
    })
}

pub(crate) fn project_conversation_file_references(
    snapshot: &ConversationSnapshot,
) -> RuntimeResult<Vec<ConversationFileReference>> {
    let results = scoped_tool_results(snapshot);
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
            let input = project_tool_input(call.name.as_str(), &call.arguments, None);
            let files = project_tool_files(
                &call.id,
                &input.value,
                results
                    .get(&(assistant.id.as_str(), call.id.as_str()))
                    .copied(),
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
    let mut files = Vec::new();
    // 批量结果逐项取可靠 success；整 Tool Result 的 Error 不抹去部分完成事实。
    if let Some(entries) = result
        .content
        .as_single_json()
        .and_then(|json| json.get("files"))
        .and_then(serde_json::Value::as_array)
        && matches!(input, ToolInputSnapshot::File { operation, .. } | ToolInputSnapshot::Files { operation, .. } if operation == "edit_file")
    {
        for (index, entry) in entries.iter().enumerate() {
            if entry.get("status").and_then(serde_json::Value::as_str) != Some("success") {
                continue;
            }
            let Some(path) = entry.get("path").and_then(serde_json::Value::as_str) else {
                continue;
            };
            files.push(workspace_tool_file(
                tool_batch_resource_ref_id(call_id, index),
                path,
                workspace_resources_available,
            )?);
        }
    } else if let ToolInputSnapshot::File { operation, path } = input
        && operation != "read_image"
        && result.status == ToolResultStatus::Success
    {
        files.push(workspace_tool_file(
            tool_resource_ref_id(call_id),
            path,
            workspace_resources_available,
        )?);
    }
    if result.status != ToolResultStatus::Success {
        return Ok(files);
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

fn workspace_tool_file(
    resource_ref_id: Option<ResourceRefId>,
    path: &str,
    available: bool,
) -> RuntimeResult<ToolFileReference> {
    Ok(ToolFileReference {
        resource_ref_id: resource_ref_id.ok_or(RuntimeError::InternalStateUnavailable {
            component: "tool resource reference",
        })?,
        origin: ToolFileResourceOrigin::WorkspaceFile,
        display_name: std::path::Path::new(path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or(path)
            .to_owned(),
        display_path: safe_display_path(path),
        size_bytes: None,
        media_type: preview_media_type(path).map(str::to_owned),
        state: if available {
            ToolFileResourceState::Available
        } else {
            ToolFileResourceState::Unavailable
        },
    })
}

fn tool_batch_resource_ref_id(
    call_id: &agent_types::ToolCallId,
    index: usize,
) -> Option<ResourceRefId> {
    ResourceRefId::new(format!("tool-file-{}-{index}", call_id.as_str())).ok()
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

fn project_mcp_identity(
    tool_name: &str,
    arguments: &serde_json::Value,
    identities: &HashMap<(String, String), McpToolIdentity>,
) -> Option<McpToolIdentity> {
    if tool_name != "call_mcp_tool" {
        return None;
    }
    let server = arguments.get("server")?.as_str()?;
    let tool = arguments.get("tool")?.as_str()?;
    identities
        .get(&(server.to_owned(), tool.to_owned()))
        .cloned()
        .or_else(|| {
            assistant_protocol::McpServerKey::new(server)
                .ok()
                .map(|server_key| McpToolIdentity {
                    server_key,
                    server_display_name: server.to_owned(),
                    tool_name: tool.to_owned(),
                })
        })
}

fn project_image_inspection_detail(
    result: &agent_types::ToolResult,
) -> Option<assistant_protocol::ImageInspectionDetailSnapshot> {
    let metadata = result.metadata.as_deref()?;
    Some(assistant_protocol::ImageInspectionDetailSnapshot {
        auxiliary_model: assistant_protocol::ModelSelection {
            provider_instance_id: assistant_protocol::ProviderInstanceId::new(
                metadata.model_provider.clone()?,
            )
            .ok()?,
            model_id: metadata.model_id.clone()?,
        },
        elapsed_ms: metadata.elapsed_ms?,
        usage: metadata.usage.as_ref().map(token_usage),
    })
}

fn project_usage(
    stored: &crate::StoredSessionUsage,
    context_window: Option<u64>,
    projected_context_tokens: Option<u64>,
    mode: agent_model::ContextManagementMode,
) -> SessionUsageSnapshot {
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
    let context = project_context_usage(projected_context_tokens, context_window, mode);
    SessionUsageSnapshot {
        accumulated,
        previous_turn: previous.map(usage_totals),
        latest_cache_hit_basis_points,
        overall_cache_hit_basis_points,
        auxiliary: (stored.auxiliary_request_count > 0).then_some(
            assistant_protocol::AuxiliaryUsageSnapshot {
                request_count: stored.auxiliary_request_count,
                input_tokens: stored.auxiliary_input_tokens,
                output_tokens: stored.auxiliary_output_tokens,
                total_tokens: stored.auxiliary_total_tokens,
            },
        ),
        context,
    }
}

#[cfg(test)]
fn context_usage_from_product_history(
    history: &ConversationSnapshot,
    mode: agent_model::ContextManagementMode,
) -> Option<u64> {
    match mode {
        agent_model::ContextManagementMode::LocalSummary => agent_context::context_token_usage(
            &crate::execution_context_from_product_history(history),
        )
        .total_tokens(),
        agent_model::ContextManagementMode::ServerCompaction => {
            agent_context::context_token_usage(history).total_tokens()
        }
    }
}

fn apply_current_context_measurement(
    context: &mut Option<assistant_protocol::ContextUsageSnapshot>,
    session: &crate::session::SessionController,
    owner: &str,
    history: &ConversationSnapshot,
) -> RuntimeResult<()> {
    let Some(context) = context.as_mut() else {
        return Ok(());
    };
    let boundary = history.messages.iter().rposition(|message| {
        matches!(
            message,
            ConversationMessage::ContextSummary(_)
                | ConversationMessage::Assistant(agent_types::AssistantMessage {
                    purpose: agent_types::AssistantMessagePurpose::ContextCompaction,
                    ..
                })
        )
    });
    let normal = history.messages.iter().rposition(|message| matches!(message,
        ConversationMessage::Assistant(assistant) if assistant.purpose == agent_types::AssistantMessagePurpose::ModelResponse && assistant.usage.is_some()
    ));
    context.calibration_pending = boundary
        .filter(|boundary| normal.is_none_or(|normal| normal < *boundary))
        .map(|_| true);
    let hash = crate::session::context_history_hash(history);
    let state = session.lock_state()?;
    if let Some(measurement) = state.context_measurements.get(owner)
        && hash == Some(measurement.history_hash)
        && measurement.session_binding == state.server_history_key
    {
        context.accuracy = assistant_protocol::ContextUsageAccuracy::Exact;
        context.calibration_pending = None;
        context.used_tokens = Some(measurement.input_tokens);
        context.usage_basis_points = Some(
            ((measurement.input_tokens.saturating_mul(10_000) / context.window_tokens).min(10_000))
                as u16,
        );
    }
    Ok(())
}

fn project_context_usage(
    used: Option<u64>,
    window: Option<u64>,
    mode: agent_model::ContextManagementMode,
) -> Option<assistant_protocol::ContextUsageSnapshot> {
    let window_tokens = window.filter(|value| *value > 0)?;
    Some(assistant_protocol::ContextUsageSnapshot {
        calibration_pending: None,
        accuracy: if used.is_some() {
            assistant_protocol::ContextUsageAccuracy::Estimated
        } else {
            assistant_protocol::ContextUsageAccuracy::Unknown
        },
        mode: match mode {
            agent_model::ContextManagementMode::LocalSummary => {
                assistant_protocol::ContextManagementModeSnapshot::LocalSummary
            }
            agent_model::ContextManagementMode::ServerCompaction => {
                assistant_protocol::ContextManagementModeSnapshot::ServerCompaction
            }
        },
        used_tokens: used,
        window_tokens,
        usage_basis_points: used.map(|value| {
            u16::try_from((value.saturating_mul(10_000) / window_tokens).min(10_000))
                .unwrap_or(10_000)
        }),
    })
}

#[cfg(test)]
pub(super) fn project_child_usage(
    snapshot: &ConversationSnapshot,
    context_window: Option<u64>,
    mode: agent_model::ContextManagementMode,
) -> ChildTaskUsageSnapshot {
    // 累计从完整历史求和，窗口只读取最后摘要之后的有效上下文。
    let used = context_usage_from_product_history(snapshot, mode);
    let usages = snapshot.messages.iter().flat_map(|message| match message {
        ConversationMessage::Assistant(message) => message.usage.iter().collect::<Vec<_>>(),
        ConversationMessage::ContextSummary(message) => message.usage.iter().collect::<Vec<_>>(),
        _ => Vec::new(),
    });
    ChildTaskUsageSnapshot {
        context: project_context_usage(used, context_window, mode),
        accumulated: sum_usage(usages),
    }
}

#[cfg(test)]
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

fn project_input_source(
    cross_session: Option<&crate::CrossSessionInputEnvelope>,
    channel_source: Option<&InputChannelSource>,
    device_names: &HashMap<assistant_protocol::DeviceId, String>,
) -> ConversationInputSourceSnapshot {
    match cross_session {
        None => match channel_source {
            Some(InputChannelSource::Desktop(source)) => ConversationInputSourceSnapshot::Desktop {
                modality: project_input_modality(source.modality),
                requested_output: project_output_preference(source.requested_output),
            },
            Some(InputChannelSource::Device(source)) => ConversationInputSourceSnapshot::Device {
                device_id: source.device_id.clone(),
                device_name: device_names
                    .get(&source.device_id)
                    .cloned()
                    .unwrap_or_else(|| "已移除设备".to_owned()),
                modality: project_input_modality(source.modality),
                requested_output: project_output_preference(source.requested_output),
            },
            None => ConversationInputSourceSnapshot::User,
        },
        Some(envelope) => match &envelope.binding {
            CrossSessionInputBinding::ControllerDelivery {
                controller_session_id,
                controller_run_id,
                ..
            } => ConversationInputSourceSnapshot::ControllerDelivery {
                controller_session_id: controller_session_id.clone(),
                controller_run_id: controller_run_id.clone(),
            },
            CrossSessionInputBinding::ProxyReport {
                source_session_id,
                source_run_id,
                source_goal_id,
                source_run_status,
            } => ConversationInputSourceSnapshot::ProxyReport {
                source_session_id: source_session_id.clone(),
                source_run_id: source_run_id.clone(),
                source_goal_id: source_goal_id.clone(),
                source_run_status: *source_run_status,
            },
        },
    }
}

fn project_input_modality(value: InputModality) -> assistant_protocol::InputModalitySnapshot {
    match value {
        InputModality::Text => assistant_protocol::InputModalitySnapshot::Text,
        InputModality::SpeechTranscript => {
            assistant_protocol::InputModalitySnapshot::SpeechTranscript
        }
    }
}

fn project_output_preference(
    value: OutputPreference,
) -> assistant_protocol::OutputPreferenceSnapshot {
    match value {
        OutputPreference::Text => assistant_protocol::OutputPreferenceSnapshot::Text,
        OutputPreference::Audio => assistant_protocol::OutputPreferenceSnapshot::Audio,
        OutputPreference::TextAndAudio => {
            assistant_protocol::OutputPreferenceSnapshot::TextAndAudio
        }
    }
}

fn user_text(message: &agent_types::UserMessage) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            UserPart::Text(text) => Some(text.text.as_str()),
            UserPart::Injected(_)
            | UserPart::InternalContext(_)
            | UserPart::QuotedText(_)
            | UserPart::FileReferences(_) => None,
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

#[cfg(test)]
mod tool_image_projection_tests {
    use agent_types::{
        AssistantMessage, FinishReason, ModelIdentity, ProviderId, ToolCall, ToolMessage, ToolName,
        ToolResult, ToolResultContent,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn image_inspection_preserves_provider_identity_and_arbitrary_model_ids() {
        for provider in ["provider-one", "provider-two"] {
            let result = ToolResult {
                call_id: agent_types::ToolCallId::new("inspect-call").unwrap(),
                status: ToolResultStatus::Success,
                content: ToolResultContent::text("image description"),
                metadata: Some(Box::new(agent_types::ToolExecutionMetadata {
                    model_provider: Some(provider.into()),
                    model_id: Some("org/vision:latest".into()),
                    elapsed_ms: Some(12),
                    usage: None,
                })),
            };
            let stored = serde_json::to_value(&result).unwrap();
            assert!(stored["metadata"].get("model_key").is_none());
            let restored: ToolResult = serde_json::from_value(stored).unwrap();
            let detail = project_image_inspection_detail(&restored).unwrap();
            assert_eq!(
                detail.auxiliary_model.provider_instance_id.as_str(),
                provider
            );
            assert_eq!(detail.auxiliary_model.model_id, "org/vision:latest");
            assert_eq!(detail.elapsed_ms, 12);
        }
        // 旧标识只按未知字段丢弃，不转换为默认模型，不影响历史工具正文读取。
        let legacy: agent_types::ToolExecutionMetadata = serde_json::from_value(json!({
            "model_key": "old-invalid/model", "elapsed_ms": 9, "usage": null,
        }))
        .unwrap();
        assert!(legacy.model_provider.is_none());
        assert!(legacy.model_id.is_none());
        assert_eq!(legacy.elapsed_ms, Some(9));
        assert!(
            serde_json::to_value(legacy)
                .unwrap()
                .get("model_key")
                .is_none()
        );
    }

    #[test]
    fn batch_edit_partial_errors_only_project_successful_positions_and_keep_legacy_refs() {
        let call_id = agent_types::ToolCallId::new("edit-call").unwrap();
        let input = ToolInputSnapshot::Files {
            operation: "edit_file".into(),
            paths: vec![
                "/workspace/a".into(),
                "/workspace/b".into(),
                "/workspace/c".into(),
                "/workspace/d".into(),
            ],
        };
        let result = ToolResult {
            call_id: call_id.clone(),
            status: ToolResultStatus::Error,
            metadata: None,
            content: ToolResultContent::json(json!({"files":[
                {"path":"/workspace/a","status":"success","replacements":2},
                {"path":"/workspace/b","status":"failed","reason":"no match"},
                {"path":"/workspace/c","status":"unknown","reason":"I/O"},
                {"path":"/workspace/d","status":"not_executed","reason":"previous_failure"}
            ]})),
        };
        let files = project_tool_files(&call_id, &input, Some(&result), true).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].resource_ref_id.as_str(), "tool-file-edit-call-0");
        assert_eq!(files[0].state, ToolFileResourceState::Available);
        let old_input = ToolInputSnapshot::File {
            operation: "edit_file".into(),
            path: "/workspace/old".into(),
        };
        let old_result = ToolResult {
            status: ToolResultStatus::Success,
            content: ToolResultContent::json(json!({"path":"/workspace/old","replacements":1})),
            ..result
        };
        let old = project_tool_files(&call_id, &old_input, Some(&old_result), true).unwrap();
        assert_eq!(old[0].resource_ref_id.as_str(), "tool-edit-call");
        assert_eq!(old[0].display_name, "old");
    }

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
            purpose: agent_types::AssistantMessagePurpose::ModelResponse,
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
            &HashMap::new(),
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
    #[test]
    fn reused_tool_ids_resolve_against_their_original_assistant_message() {
        let (mut snapshot, first_id, call_id) = image_exchange();
        let mut second = snapshot.clone();
        let ConversationMessage::Assistant(assistant) = &mut second.messages[0] else {
            unreachable!()
        };
        assistant.id = agent_types::MessageId::new("second-image-exchange").unwrap();
        let ConversationMessage::Tool(tool) = &mut second.messages[1] else {
            unreachable!()
        };
        tool.id = agent_types::MessageId::new("second-tool-result").unwrap();
        tool.result.content = agent_types::ToolResultContent::text("second result");
        snapshot.messages.extend(second.messages);
        let results = scoped_tool_results(&snapshot);
        assert!(
            results
                .get(&(first_id.as_str(), call_id.as_str()))
                .unwrap()
                .content
                .as_parts()
                .iter()
                .any(|part| matches!(part, agent_types::ToolResultPart::Image { .. }))
        );
        assert_eq!(
            results
                .get(&("second-image-exchange", call_id.as_str()))
                .unwrap()
                .content
                .as_parts(),
            &[agent_types::ToolResultPart::text("second result")]
        );
    }
}
