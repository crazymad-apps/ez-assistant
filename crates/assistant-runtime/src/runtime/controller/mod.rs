//! 主控 Run 的最小跨会话工具与权威执行用例。
//!
//! 本模块刻意分开三种阶段：主控工具发起命令、源 Run 结算前准备代理报告、源 Run 事务提交后
//! 把报告投影到主控 Session。跨 Session 流程任何时刻只获取一个 Session mutation gate；
//! Store 是权威事实，内存投影和 RuntimeEvent 只在可靠提交后更新。

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use agent_types::UserMessageOrigin;
use assistant_protocol::{
    GoalId, IdempotencyKey, InputId, RunId, RunStatus, RuntimeErrorInfo, RuntimeEvent, SessionId,
    ToolCallId,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use crate::{
    CrossSessionInputBinding, CrossSessionInputEnvelope, InputChannelSource, InputOrigin,
    NewStoredInput, ReplyRoute, RuntimeError, RuntimeResult, RuntimeStore, SessionProxyChange,
    SessionProxyState, SessionRole, StoredInputState,
    config::ConfigRegistry,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    observation::ObservationCoordinator,
    run::{allocate_run_id, create_user_message},
    session::SessionController,
};

use super::{
    goal::{ensure_goal_model_supported, prepare_goal_start},
    input::projection::{AcceptedInputProjection, project_accepted_input},
};

mod tool;

pub(crate) use tool::ControllerAuthorizationFacts;
pub(super) use tool::controller_tool_set;

const MAX_MANAGED_SESSIONS: usize = 100;
const MAX_CROSS_SESSION_INPUT_BYTES: usize = 64 * 1024;
const MAX_PROXY_REPORT_RESULT_BYTES: usize = 48 * 1024;

type WakeQueue = dyn Fn(Arc<SessionController>) -> RuntimeResult<()> + Send + Sync;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) struct ManagedSession {
    pub session_id: String,
    pub title: String,
    pub workspace_id: Option<String>,
    pub workspace_label: Option<String>,
    pub workspace_primary_directory: Option<String>,
    pub workspace_additional_directories: Vec<String>,
    pub proxy_enabled: bool,
    pub can_accept_message: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(super) struct DeliveryReceipt {
    pub target_session_id: String,
    pub input_id: String,
    pub run_id: String,
    pub status: &'static str,
}

pub(crate) struct ProxyReportDraft {
    pub(crate) source_session_id: SessionId,
    pub(crate) source_title: String,
    pub(crate) source_run_id: RunId,
    pub(crate) source_goal_id: Option<GoalId>,
    pub(crate) goal_summary: Option<String>,
    pub(crate) source_run_status: RunStatus,
    pub(crate) controller_session_id: SessionId,
    pub(crate) final_text: Option<String>,
    pub(crate) error: Option<RuntimeErrorInfo>,
    pub(crate) accepted_at_ms: i64,
    pub(crate) reply_route: ReplyRoute,
}

pub(crate) fn reply_route_for_input(input: &crate::StoredInput) -> ReplyRoute {
    if let Some(envelope) = input.cross_session.as_ref()
        && matches!(
            envelope.binding,
            CrossSessionInputBinding::ControllerDelivery { .. }
        )
    {
        return envelope.reply_route.clone();
    }
    input
        .goal_binding
        .as_ref()
        .and_then(|binding| binding.reply_route.clone())
        .unwrap_or_default()
}

/// 工具实例共享的 Runtime 私有协调器；不持有 Controller Session mutation gate。
pub(crate) struct ControllerToolCoordinator {
    sessions: Arc<RwLock<BTreeMap<SessionId, Arc<SessionController>>>>,
    workspaces: Arc<RwLock<BTreeMap<assistant_protocol::WorkspaceId, crate::StoredWorkspace>>>,
    config_registry: Arc<ConfigRegistry>,
    store: Arc<dyn RuntimeStore>,
    events: ObservationCoordinator,
    wake_queue: Arc<WakeQueue>,
}

impl ControllerToolCoordinator {
    pub(super) fn new(
        sessions: Arc<RwLock<BTreeMap<SessionId, Arc<SessionController>>>>,
        workspaces: Arc<RwLock<BTreeMap<assistant_protocol::WorkspaceId, crate::StoredWorkspace>>>,
        config_registry: Arc<ConfigRegistry>,
        store: Arc<dyn RuntimeStore>,
        events: ObservationCoordinator,
        wake_queue: Arc<WakeQueue>,
    ) -> Self {
        Self {
            sessions,
            workspaces,
            config_registry,
            store,
            events,
            wake_queue,
        }
    }

    pub(super) fn list_managed_sessions(
        &self,
        controller_session_id: &SessionId,
    ) -> RuntimeResult<Vec<ManagedSession>> {
        self.ensure_current_controller(controller_session_id)?;
        let sessions = self.session_values()?;
        let workspaces =
            self.workspaces
                .read()
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "workspace registry",
                })?;
        let mut managed = sessions
            .into_iter()
            .filter_map(|session| {
                let state = session.lock_state().ok()?;
                if state.role != SessionRole::Standard
                    || state.lifecycle != assistant_protocol::SessionLifecycle::Active
                {
                    return None;
                }
                let proxy_enabled = state
                    .proxy
                    .as_ref()
                    .is_some_and(|proxy| proxy.controller_session_id == *controller_session_id);
                Some(ManagedSession {
                    session_id: session.id().as_str().to_owned(),
                    title: state.title.clone(),
                    workspace_id: session
                        .environment()
                        .workspace_id
                        .as_ref()
                        .map(|id| id.as_str().to_owned()),
                    workspace_label: session
                        .environment()
                        .workspace_id
                        .as_ref()
                        .and_then(|id| workspaces.get(id))
                        .map(|workspace| workspace.label.clone()),
                    workspace_primary_directory: session
                        .environment()
                        .workspace_id
                        .as_ref()
                        .map(|_| session.environment().working_directory.clone()),
                    workspace_additional_directories: session
                        .environment()
                        .additional_workspace_directories
                        .clone(),
                    proxy_enabled,
                    can_accept_message: proxy_enabled
                        && !state
                            .inputs
                            .values()
                            .any(|input| input.stored.state == StoredInputState::Queued),
                })
            })
            .collect::<Vec<_>>();
        managed.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        managed.truncate(MAX_MANAGED_SESSIONS);
        Ok(managed)
    }

    pub(super) async fn set_proxy(
        &self,
        controller_session_id: &SessionId,
        target_session_id: &SessionId,
        enabled: bool,
    ) -> RuntimeResult<bool> {
        self.ensure_current_controller(controller_session_id)?;
        let target = self.session(target_session_id)?;
        let _mutation = target.mutation().await;
        target.ensure_healthy()?;
        target.ensure_active()?;
        target.ensure_standard_role()?;
        let current = target.lock_state()?.proxy.clone();
        let already_expected = match (&current, enabled) {
            (Some(proxy), true) => proxy.controller_session_id == *controller_session_id,
            (None, false) => true,
            _ => false,
        };
        if already_expected {
            return Ok(false);
        }
        let changed_at_ms = super::now_ms()?;
        self.store
            .set_session_proxy(SessionProxyChange {
                target_session_id: target_session_id.clone(),
                controller_session_id: controller_session_id.clone(),
                enabled,
                changed_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("set session proxy", source))?;
        target.lock_state()?.proxy = enabled.then(|| SessionProxyState {
            controller_session_id: controller_session_id.clone(),
            changed_at_ms,
        });
        let _ = self.events.send(RuntimeEvent::SessionChanged {
            session_id: target_session_id.clone(),
        });
        Ok(true)
    }

    /// 执行主控 `send_session_message` 工具的权威投递用例。
    ///
    /// 本函数只获取目标 Session mutation gate：先校验当前主控、代理关系和空 Queue，再由 Store
    /// 原子接受 `ControllerDelivery` Input 与首次 Run；`start_goal` 为真时，同一事务还创建 Goal
    /// 并冻结其回复路径。Store 成功后复用统一 Input/Goal 投影并唤醒目标；同一主控 ToolCall 的
    /// 重放返回首次 receipt，不重新入队或重复创建 Goal。
    pub(super) async fn deliver(
        &self,
        controller_session_id: &SessionId,
        controller_run_id: &RunId,
        controller_tool_call_id: &ToolCallId,
        target_session_id: &SessionId,
        message_text: String,
        start_goal: bool,
    ) -> RuntimeResult<DeliveryReceipt> {
        self.ensure_current_controller(controller_session_id)?;
        let message_text = message_text.trim().to_owned();
        if message_text.is_empty() || message_text.len() > MAX_CROSS_SESSION_INPUT_BYTES {
            return Err(RuntimeError::InvalidRequest {
                reason: "controller message must be non-empty and within the input limit",
            });
        }
        let target = self.session(target_session_id)?;
        let _mutation = target.mutation().await;
        target.ensure_healthy()?;
        target.ensure_active()?;
        target.ensure_standard_role()?;
        let idempotency_key = delivery_idempotency_key(
            controller_session_id,
            controller_run_id,
            controller_tool_call_id,
        )?;
        let (variant, approval_mode, input_id, run_id, model_key) = {
            let state = target.lock_state()?;
            if let Some(existing) = state
                .inputs
                .values()
                .find(|input| input.stored.idempotency_key.as_ref() == Some(&idempotency_key))
            {
                return Ok(DeliveryReceipt {
                    target_session_id: target_session_id.as_str().to_owned(),
                    input_id: existing.stored.input_id.as_str().to_owned(),
                    run_id: existing.first_run_id.as_str().to_owned(),
                    status: "already_accepted",
                });
            }
            if state
                .proxy
                .as_ref()
                .map(|proxy| &proxy.controller_session_id)
                != Some(controller_session_id)
                || state
                    .inputs
                    .values()
                    .any(|input| input.stored.state == StoredInputState::Queued)
            {
                return Err(RuntimeError::SessionBusy {
                    session_id: target_session_id.clone(),
                });
            }
            if start_goal && state.goal.is_some() {
                return Err(RuntimeError::GoalAlreadyExists {
                    session_id: target_session_id.clone(),
                });
            }
            (
                state.current_variant,
                state.approval_mode,
                allocate_input_id(&state)?,
                allocate_run_id(&state)?,
                state.model_key.clone(),
            )
        };
        if start_goal {
            ensure_goal_model_supported(&self.config_registry, target.as_ref(), &model_key)?;
        }
        let reply_route = self.reply_route(controller_session_id, controller_run_id)?;
        let mut message = create_user_message(message_text, Vec::new(), variant)?;
        message.origin = UserMessageOrigin::Runtime;
        InternalBoundaryCoordinator::append(
            &mut message,
            InternalBoundaryRequest {
                source: InternalBoundarySource::ControllerDelivery,
                text: format!(
                    "This visible task was delivered by controller session {} from run {}.",
                    controller_session_id.as_str(),
                    controller_run_id.as_str()
                ),
            },
        )?;
        let envelope = CrossSessionInputEnvelope {
            binding: CrossSessionInputBinding::ControllerDelivery {
                controller_session_id: controller_session_id.clone(),
                controller_run_id: controller_run_id.clone(),
                controller_tool_call_id: controller_tool_call_id.clone(),
            },
            reply_route: reply_route.clone(),
        };
        let accepted_at_ms = super::now_ms()?;
        let prepared_goal = start_goal
            .then(|| prepare_goal_start(&mut message, accepted_at_ms, Some(reply_route)))
            .transpose()?;
        let goal_binding = prepared_goal.as_ref().map(|goal| goal.binding.clone());
        let new_goal = prepared_goal
            .as_ref()
            .map(|goal| goal.control.to_stored(target_session_id.clone()));
        let goal_snapshot = prepared_goal
            .as_ref()
            .map(|goal| super::product::project_goal(&goal.control))
            .transpose()?;
        let accepted = self
            .store
            .accept_input(NewStoredInput {
                input_id,
                run_id,
                session_id: target_session_id.clone(),
                idempotency_key: Some(idempotency_key),
                agent_variant: variant,
                origin: InputOrigin::Runtime,
                goal_binding,
                cross_session: Some(envelope),
                channel_source: None,
                skill_activation: None,
                mcp_selection: None,
                approval_mode,
                message,
                new_goal,
                resumed_goal: None,
                generated_title: None,
                accepted_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("deliver controller message", source))?;
        let receipt = DeliveryReceipt {
            target_session_id: target_session_id.as_str().to_owned(),
            input_id: accepted.input.input_id.as_str().to_owned(),
            run_id: accepted.run.run_id.as_str().to_owned(),
            status: if accepted.is_duplicate {
                "already_accepted"
            } else {
                "accepted"
            },
        };
        if !accepted.is_duplicate {
            let projection = {
                let mut state = target.lock_state()?;
                if let Some(goal) = prepared_goal {
                    state.goal = Some(goal.control);
                }
                project_accepted_input(&mut state, accepted, None)
            };
            self.publish_and_wake_projected_input(&target, projection, goal_snapshot)?;
        }
        Ok(receipt)
    }

    /// 在源 Run 结算事务开始前构造代理报告 Input，但不持久化或修改主控 Session。
    ///
    /// 调用方仍持有源 Session mutation gate，因此这里只短暂读取主控 Session 当前变体和审批模式，
    /// 并分配报告 Input/Run ID。返回值随后作为源 Run settlement 的可选 effect 原子提交。
    pub(crate) fn prepare_proxy_report(
        &self,
        draft: ProxyReportDraft,
    ) -> RuntimeResult<NewStoredInput> {
        let target = self.session(&draft.controller_session_id)?;
        target.ensure_healthy()?;
        target.ensure_active()?;
        let (variant, approval_mode, input_id, run_id) = {
            let state = target.lock_state()?;
            if state.role != SessionRole::Controller {
                return Err(RuntimeError::ControllerUnavailable);
            }
            (
                state.current_variant,
                state.approval_mode,
                allocate_input_id(&state)?,
                allocate_run_id(&state)?,
            )
        };
        build_proxy_report_input(draft, variant, approval_mode, input_id, run_id)
    }

    /// 将已经随源 Run settlement 落库的代理报告投影到主控 Session，并启动其执行。
    ///
    /// `accepted` 不是待提交命令，而是 Store 已可靠接受的 `ProxyReport` Input 与首次 Run；本函数
    /// 不再写 Store。调用方必须先完成源 Session 的内存结算并释放源 mutation gate，本函数随后只取
    /// 主控 Session gate，避免同时持有两个 Session gate。若崩溃恢复或安全重试发现同一 Input 已在
    /// 内存中，则直接返回；否则完成统一 Session lane 投影，再发布可丢失的观察事件并唤醒 Queue。
    pub(crate) async fn project_proxy_report(
        &self,
        accepted: crate::AcceptedInput,
    ) -> RuntimeResult<()> {
        let target = self.session(&accepted.input.session_id)?;
        let _mutation = target.mutation().await;
        let projection = {
            let mut state = target.lock_state()?;
            if state.inputs.contains_key(&accepted.input.input_id) {
                return Ok(());
            }
            project_accepted_input(&mut state, accepted, None)
        };
        // 观察事件和 Queue 唤醒不属于内存投影临界区；先释放主控 gate，也让后续 driver 能取得它。
        drop(_mutation);
        self.publish_and_wake_projected_input(&target, projection, None)
    }

    /// 发布 Store 成功后的观察事件并唤醒目标 Queue；事件丢失可由 Session 快照恢复。
    fn publish_and_wake_projected_input(
        &self,
        target: &Arc<SessionController>,
        projection: AcceptedInputProjection,
        goal: Option<assistant_protocol::GoalSnapshot>,
    ) -> RuntimeResult<()> {
        let _ = self.events.send(RuntimeEvent::RunAccepted {
            session_id: target.id().clone(),
            run_id: projection.run.run_id,
        });
        if let Some(goal) = goal {
            let _ = self.events.send(RuntimeEvent::GoalChanged {
                session_id: target.id().clone(),
                goal_id: goal.goal_id,
                generation: goal.generation,
            });
        }
        if let Some(revision) = projection.queue_revision {
            let _ = self.events.send(RuntimeEvent::QueueChanged {
                session_id: target.id().clone(),
                revision,
            });
        }
        (self.wake_queue)(target.clone())
    }

    fn ensure_current_controller(&self, session_id: &SessionId) -> RuntimeResult<()> {
        let mut controllers = self
            .session_values()?
            .into_iter()
            .filter(|session| session.role().ok() == Some(SessionRole::Controller))
            .collect::<Vec<_>>();
        controllers.sort_by(|left, right| {
            left.created_at_ms()
                .cmp(&right.created_at_ms())
                .then_with(|| left.id().cmp(right.id()))
        });
        if controllers.first().map(|session| session.id()) != Some(session_id) {
            return Err(RuntimeError::ControllerUnavailable);
        }
        Ok(())
    }

    fn session_values(&self) -> RuntimeResult<Vec<Arc<SessionController>>> {
        self.sessions
            .read()
            .map(|sessions| sessions.values().cloned().collect())
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })
    }

    fn session(&self, session_id: &SessionId) -> RuntimeResult<Arc<SessionController>> {
        self.sessions
            .read()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session registry",
            })?
            .get(session_id)
            .cloned()
            .ok_or_else(|| RuntimeError::SessionNotFound {
                session_id: session_id.clone(),
            })
    }
}

pub(crate) fn build_proxy_report_input(
    draft: ProxyReportDraft,
    variant: assistant_protocol::AgentVariant,
    approval_mode: assistant_protocol::ApprovalMode,
    input_id: InputId,
    run_id: RunId,
) -> RuntimeResult<NewStoredInput> {
    let text = proxy_report_text(&draft);
    let mut message = create_user_message(text, Vec::new(), variant)?;
    message.origin = UserMessageOrigin::Runtime;
    InternalBoundaryCoordinator::append(
        &mut message,
        InternalBoundaryRequest {
            source: InternalBoundarySource::ProxyReport,
            text: format!(
                "This visible report was emitted by managed session {} from run {} with terminal status {}.",
                draft.source_session_id.as_str(),
                draft.source_run_id.as_str(),
                run_status_label(draft.source_run_status),
            ),
        },
    )?;
    let idempotency_key =
        proxy_report_idempotency_key(&draft.source_session_id, &draft.source_run_id)?;
    Ok(NewStoredInput {
        input_id,
        run_id,
        session_id: draft.controller_session_id,
        idempotency_key: Some(idempotency_key),
        agent_variant: variant,
        origin: InputOrigin::Runtime,
        goal_binding: None,
        cross_session: Some(CrossSessionInputEnvelope {
            binding: CrossSessionInputBinding::ProxyReport {
                source_session_id: draft.source_session_id,
                source_run_id: draft.source_run_id,
                source_goal_id: draft.source_goal_id,
                source_run_status: draft.source_run_status,
            },
            reply_route: draft.reply_route,
        }),
        channel_source: None,
        skill_activation: None,
        mcp_selection: None,
        approval_mode,
        message,
        new_goal: None,
        resumed_goal: None,
        generated_title: None,
        accepted_at_ms: draft.accepted_at_ms,
    })
}

impl ControllerToolCoordinator {
    fn reply_route(
        &self,
        controller_session_id: &SessionId,
        controller_run_id: &RunId,
    ) -> RuntimeResult<ReplyRoute> {
        let controller = self.session(controller_session_id)?;
        let state = controller.lock_state()?;
        let run =
            state
                .runs
                .get(controller_run_id)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "controller delivery source run",
                })?;
        let input =
            state
                .inputs
                .get(run.input_id())
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "controller delivery source input",
                })?;
        Ok(match input.stored.channel_source.as_ref() {
            Some(InputChannelSource::Device(source)) => ReplyRoute::Device {
                device_id: source.device_id.clone(),
                requested_output: source.requested_output,
            },
            _ => ReplyRoute::SessionDefault,
        })
    }
}

fn allocate_input_id(state: &crate::session::SessionState) -> RuntimeResult<InputId> {
    for _ in 0..8 {
        let value =
            crate::id::generate("input").map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "input id random source",
            })?;
        let id = InputId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "input id generator",
        })?;
        if !state.inputs.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "input id collision",
    })
}

fn delivery_idempotency_key(
    controller_session_id: &SessionId,
    controller_run_id: &RunId,
    controller_tool_call_id: &ToolCallId,
) -> RuntimeResult<IdempotencyKey> {
    let mut hasher = Sha256::new();
    hasher.update(b"controller-delivery-v1\0");
    hasher.update(controller_session_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(controller_run_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(controller_tool_call_id.as_str().as_bytes());
    IdempotencyKey::new(format!("cd_{}", URL_SAFE_NO_PAD.encode(hasher.finalize()))).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "controller delivery idempotency key",
        }
    })
}

fn proxy_report_idempotency_key(
    source_session_id: &SessionId,
    source_run_id: &RunId,
) -> RuntimeResult<IdempotencyKey> {
    let mut hasher = Sha256::new();
    hasher.update(b"proxy-report-v1\0");
    hasher.update(source_session_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(source_run_id.as_str().as_bytes());
    IdempotencyKey::new(format!("pr_{}", URL_SAFE_NO_PAD.encode(hasher.finalize()))).map_err(|_| {
        RuntimeError::InternalStateUnavailable {
            component: "proxy report idempotency key",
        }
    })
}

fn proxy_report_text(draft: &ProxyReportDraft) -> String {
    let mut text = format!(
        "Managed session report\nSession: {} ({})\nRun: {}\nStatus: {}",
        draft.source_title,
        draft.source_session_id.as_str(),
        draft.source_run_id.as_str(),
        run_status_label(draft.source_run_status),
    );
    if let Some(goal_id) = draft.source_goal_id.as_ref() {
        text.push_str("\nGoal: ");
        text.push_str(goal_id.as_str());
    }
    if let Some(summary) = draft.goal_summary.as_deref() {
        text.push_str("\nGoal status: ");
        text.push_str(summary);
    }
    if let Some(error) = draft.error.as_ref() {
        text.push_str("\nError: ");
        text.push_str(&runtime_error_code_label(error.code));
        text.push_str(" — ");
        text.push_str(&truncate_utf8(
            &error.message,
            MAX_PROXY_REPORT_RESULT_BYTES,
        ));
    } else if let Some(result) = draft.final_text.as_deref() {
        text.push_str("\nResult:\n");
        let result = truncate_utf8(result, MAX_PROXY_REPORT_RESULT_BYTES);
        text.push_str(&result);
        if result.len() < draft.final_text.as_deref().unwrap_or_default().len() {
            text.push_str("\n[truncated: true]");
        }
    }
    truncate_utf8(&text, MAX_CROSS_SESSION_INPUT_BYTES)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Interrupted => "interrupted",
        _ => "unsupported",
    }
}

fn runtime_error_code_label(code: assistant_protocol::RuntimeErrorCode) -> String {
    serde_json::to_string(&code)
        .unwrap_or_else(|_| "\"internal\"".to_owned())
        .trim_matches('"')
        .to_owned()
}
