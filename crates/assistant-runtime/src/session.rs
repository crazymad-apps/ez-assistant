//! 单个 Session 的可切换模型 key、冻结 System Prompt 与短临界区状态。

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

use agent_model::SystemPromptSnapshot;
use agent_types::ConversationSnapshot;
use assistant_protocol::{
    AgentVariant, ApprovalMode, IdempotencyKey, InputId, ModelKey, ReasoningEffortKey, RunId,
    RunSnapshot, SessionCompactionSnapshot, SessionId, SessionLifecycle, SessionSummary,
    SessionTitleGenerationSnapshot, SessionTitleOrigin,
};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::{
    PcOutputHosting, RuntimeError, RuntimeResult, RuntimeStore, SessionExecutionEnvironment,
    SessionProxyState, SessionRole, SessionSkillCatalog, StoredConversationState, StoredInput,
    StoredInputState, StoredRun, StoredSession,
    goal::{GoalControl, GoalState},
    id,
    journal::InMemoryJournal,
    run::{ActiveRun, RunRecord},
    work_plan::WorkPlan,
};

/// 单个 Session 的并发协调器和状态所有者。
///
/// `mutation_gate` 串行化跨 await 的业务变更，`state` 只保护短临界区内存投影；
/// 可靠事实必须先由 Store 提交，不能仅修改这里的状态。
pub(crate) struct SessionController {
    id: SessionId,
    created_at_ms: i64,
    system_prompt: SystemPromptSnapshot,
    skill_catalog: SessionSkillCatalog,
    environment: SessionExecutionEnvironment,
    mutation_gate: AsyncMutex<()>,
    state: Mutex<SessionState>,
}

/// Session 在当前 Runtime 进程中的完整可变投影。
///
/// 字段同时包含可恢复业务事实和明确标注的进程内状态，恢复时由 Store 重新构建而非序列化本结构。
pub(crate) struct SessionState {
    pub(crate) title: String,
    pub(crate) is_pinned: bool,
    pub(crate) title_origin: SessionTitleOrigin,
    pub(crate) model_key: ModelKey,
    pub(crate) reasoning_effort: Option<ReasoningEffortKey>,
    pub(crate) current_variant: AgentVariant,
    pub(crate) approval_mode: ApprovalMode,
    pub(crate) lifecycle: SessionLifecycle,
    pub(crate) role: SessionRole,
    pub(crate) proxy: Option<SessionProxyState>,
    pub(crate) pc_output_hosting: Option<PcOutputHosting>,
    pub(crate) journal: Option<InMemoryJournal>,
    pub(crate) persisted_message_count: usize,
    pub(crate) message_count: u64,
    pub(crate) body_generation: u64,
    pub(crate) is_conversation_available: bool,
    pub(crate) runs: BTreeMap<RunId, RunRecord>,
    pub(crate) inputs: BTreeMap<InputId, InputRecord>,
    /// 所有无 Goal binding 的 Session 输入，包括用户输入与跨会话 Runtime 输入。
    pub(crate) session_inputs: VecDeque<InputId>,
    /// 当前 Goal 专用输入；可靠状态下最多一条。
    pub(crate) goal_inputs: VecDeque<InputId>,
    pub(crate) queue_revision: u64,
    pub(crate) queue_paused_by_user: bool,
    pub(crate) resume_required: bool,
    pub(crate) is_queue_driver_running: bool,
    pub(crate) active_run: Option<ActiveRun>,
    /// 当前进程内的逻辑输出周期；重启后不恢复也不补播。
    pub(crate) output_cycle: Option<crate::OutputCycleState>,
    /// 仅属于当前 Runtime 进程；不得进入 Store 或恢复投影。
    pub(crate) active_compaction: Option<ActiveSessionCompaction>,
    /// Store 中可恢复的自动标题资格。
    pub(crate) automatic_title_pending: bool,
    /// 当前进程中唯一有效的标题旁路调用。
    pub(crate) active_title_generation: Option<ActiveSessionTitleGeneration>,
    pub(crate) is_faulted: bool,
    pub(crate) updated_at_ms: i64,
    pub(crate) archived_at_ms: Option<i64>,
    pub(crate) work_plan: Option<WorkPlan>,
    pub(crate) goal: Option<GoalControl>,
    /// 规范 Conversation 的结构化 Skill Activation 账本。
    pub(crate) skill_activations: Vec<crate::StoredSkillActivation>,
}

/// 当前正在执行的手动或自动压缩及其可选取消句柄。
///
/// 它仅用于进程内互斥与 UI 投影，压缩可靠结果仍通过 Conversation/Store 表达。
#[derive(Clone)]
pub(crate) struct ActiveSessionCompaction {
    pub(crate) snapshot: SessionCompactionSnapshot,
    pub(crate) cancellation: Option<CancellationToken>,
}

/// 当前标题旁路的受控进程内身份和取消句柄。
#[derive(Clone)]
pub(crate) struct ActiveSessionTitleGeneration {
    pub(crate) task_id: String,
    pub(crate) snapshot: SessionTitleGenerationSnapshot,
    pub(crate) cancellation: CancellationToken,
}

/// 一条已接受 Input 的持久事实及其首次、最近 Run 关联。
///
/// 重试会更新 `latest_run_id`，但不会创建第二条用户输入或改变 `first_run_id`。
#[derive(Clone)]
pub(crate) struct InputRecord {
    pub(crate) stored: StoredInput,
    pub(crate) first_run_id: RunId,
    pub(crate) latest_run_id: RunId,
}

impl SessionState {
    /// 按 Goal 状态选择唯一可自动领取的输入；Goal 存在时不会回退消费用户队列。
    pub(crate) fn next_runnable_input(&self) -> Option<InputId> {
        match self.goal.as_ref() {
            None => {
                if self.queue_paused_by_user || self.resume_required {
                    None
                } else {
                    self.session_inputs.front().cloned()
                }
            }
            Some(goal) if matches!(goal.state, GoalState::Running) => self
                .goal_inputs
                .front()
                .filter(|input_id| {
                    self.inputs
                        .get(*input_id)
                        .and_then(|input| input.stored.goal_binding.as_ref())
                        .is_some_and(|binding| {
                            binding.goal_id == goal.id && binding.generation == goal.generation
                        })
                })
                .cloned(),
            Some(_) => None,
        }
    }

    /// 移除已领取或启动失败的输入，返回它是否属于产品用户 Queue。
    pub(crate) fn pop_runnable_input(&mut self, input_id: &InputId) -> Option<bool> {
        if self.goal_inputs.front() == Some(input_id) {
            self.goal_inputs.pop_front();
            return Some(false);
        }
        if self.session_inputs.front() == Some(input_id) {
            self.session_inputs.pop_front();
            return Some(true);
        }
        None
    }
}

/// 在当前 Session Registry 中分配一个未占用的短标识。
pub(crate) fn allocate_session_id(
    sessions: &BTreeMap<SessionId, Arc<SessionController>>,
) -> RuntimeResult<SessionId> {
    for _ in 0..id::GENERATION_ATTEMPTS {
        let value = id::generate("s").map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "session id random source",
        })?;
        let id = SessionId::new(value).map_err(|_| RuntimeError::InternalStateUnavailable {
            component: "session id generator",
        })?;
        if !sessions.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(RuntimeError::InternalStateUnavailable {
        component: "session id collision",
    })
}

impl SessionController {
    pub(crate) fn new(stored: StoredSession) -> Self {
        Self {
            id: stored.session_id,
            created_at_ms: stored.created_at_ms,
            system_prompt: stored.system_prompt,
            skill_catalog: stored.skill_catalog,
            environment: stored.environment,
            mutation_gate: AsyncMutex::new(()),
            state: Mutex::new(SessionState {
                title: stored.title,
                is_pinned: stored.is_pinned,
                title_origin: stored.title_origin,
                model_key: stored.model_key,
                reasoning_effort: stored.reasoning_effort,
                current_variant: stored.current_variant,
                approval_mode: stored.approval_mode,
                lifecycle: map_lifecycle(stored.lifecycle),
                role: stored.role,
                proxy: stored.proxy,
                pc_output_hosting: stored.pc_output_hosting,
                journal: Some(InMemoryJournal::new()),
                persisted_message_count: 0,
                message_count: stored.message_count,
                body_generation: stored.body_generation,
                is_conversation_available: true,
                runs: BTreeMap::new(),
                inputs: BTreeMap::new(),
                session_inputs: VecDeque::new(),
                goal_inputs: VecDeque::new(),
                queue_revision: 0,
                queue_paused_by_user: false,
                resume_required: false,
                is_queue_driver_running: false,
                active_run: None,
                output_cycle: None,
                active_compaction: None,
                automatic_title_pending: stored.automatic_title_pending,
                active_title_generation: None,
                is_faulted: false,
                updated_at_ms: stored.updated_at_ms,
                archived_at_ms: stored.archived_at_ms,
                work_plan: None,
                goal: None,
                skill_activations: Vec::new(),
            }),
        }
    }

    pub(crate) fn recovered(
        stored: StoredSession,
        runs: Vec<StoredRun>,
        inputs: Vec<StoredInput>,
        work_plan: Option<WorkPlan>,
        goal: Option<GoalControl>,
        skill_activations: Vec<crate::StoredSkillActivation>,
    ) -> Self {
        let is_conversation_available =
            stored.conversation_state == StoredConversationState::Available;
        let run_records = runs
            .into_iter()
            .map(|run| (run.run_id.clone(), RunRecord::recovered(run)))
            .collect::<BTreeMap<_, _>>();
        let mut input_records = BTreeMap::new();
        let mut session_inputs = Vec::new();
        let mut goal_inputs = Vec::new();
        for input in inputs {
            let mut owned_runs = run_records
                .values()
                .filter(|run| run.input_id() == &input.input_id)
                .collect::<Vec<_>>();
            owned_runs.sort_by_key(|run| run.attempt());
            if let (Some(first), Some(latest)) = (owned_runs.first(), owned_runs.last()) {
                if input.state == StoredInputState::Queued
                    && latest.status() == assistant_protocol::RunStatus::Accepted
                {
                    if input.goal_binding.is_some() {
                        goal_inputs.push((input.queue_order, input.input_id.clone()));
                    } else {
                        session_inputs.push((input.queue_order, input.input_id.clone()));
                    }
                }
                input_records.insert(
                    input.input_id.clone(),
                    InputRecord {
                        stored: input,
                        first_run_id: first.snapshot().run_id,
                        latest_run_id: latest.snapshot().run_id,
                    },
                );
            }
        }
        session_inputs.sort_by_key(|(order, _)| *order);
        goal_inputs.sort_by_key(|(order, _)| *order);
        let resume_required = !session_inputs.is_empty();
        Self {
            id: stored.session_id,
            created_at_ms: stored.created_at_ms,
            system_prompt: stored.system_prompt,
            skill_catalog: stored.skill_catalog,
            environment: stored.environment,
            mutation_gate: AsyncMutex::new(()),
            state: Mutex::new(SessionState {
                title: stored.title,
                is_pinned: stored.is_pinned,
                title_origin: stored.title_origin,
                model_key: stored.model_key,
                reasoning_effort: stored.reasoning_effort,
                current_variant: stored.current_variant,
                approval_mode: stored.approval_mode,
                lifecycle: map_lifecycle(stored.lifecycle),
                role: stored.role,
                proxy: stored.proxy,
                pc_output_hosting: stored.pc_output_hosting,
                journal: None,
                persisted_message_count: 0,
                message_count: stored.message_count,
                body_generation: stored.body_generation,
                is_conversation_available,
                runs: run_records,
                inputs: input_records,
                session_inputs: session_inputs.into_iter().map(|(_, id)| id).collect(),
                goal_inputs: goal_inputs.into_iter().map(|(_, id)| id).collect(),
                queue_revision: 0,
                queue_paused_by_user: false,
                resume_required,
                is_queue_driver_running: false,
                active_run: None,
                output_cycle: None,
                active_compaction: None,
                automatic_title_pending: stored.automatic_title_pending,
                active_title_generation: None,
                is_faulted: false,
                updated_at_ms: stored.updated_at_ms,
                archived_at_ms: stored.archived_at_ms,
                work_plan,
                goal,
                skill_activations,
            }),
        }
    }

    pub(crate) fn summary(&self) -> RuntimeResult<SessionSummary> {
        let state = self.lock_state()?;
        Ok(SessionSummary {
            session_id: self.id.clone(),
            title: state.title.clone(),
            model_key: state.model_key.clone(),
            reasoning_effort: state.reasoning_effort,
            lifecycle: state.lifecycle,
            role: match state.role {
                SessionRole::Standard => assistant_protocol::SessionRoleSnapshot::Standard,
                SessionRole::Controller => assistant_protocol::SessionRoleSnapshot::Controller,
            },
            proxy: state
                .proxy
                .as_ref()
                .map(|proxy| assistant_protocol::SessionProxySnapshot {
                    controller_session_id: proxy.controller_session_id.clone(),
                    changed_at_ms: proxy.changed_at_ms,
                }),
            pc_output_hosting: state.pc_output_hosting.as_ref().map(|hosting| {
                assistant_protocol::PcOutputHostingSnapshot {
                    device_id: hosting.device_id.clone(),
                    device_name: hosting.device_name.clone(),
                }
            }),
            active_compaction: state
                .active_compaction
                .as_ref()
                .map(|active| active.snapshot.clone()),
            current_variant: state.current_variant,
            approval_mode: state.approval_mode,
            workspace_id: self.environment.workspace_id.clone(),
            active_run_id: state
                .active_run
                .as_ref()
                .map(|active| active.run_id.clone()),
            message_count: state.message_count,
            queued_input_count: state
                .inputs
                .values()
                .filter(|input| {
                    input.stored.state == StoredInputState::Queued
                        && input.stored.origin == crate::InputOrigin::User
                        && input.stored.goal_binding.is_none()
                })
                .count() as u64,
            resume_required: state.resume_required,
            created_at_ms: Some(self.created_at_ms),
            updated_at_ms: Some(state.updated_at_ms),
            archived_at_ms: state.archived_at_ms,
            is_pinned: state.is_pinned,
            title_origin: state.title_origin,
            pending_approval_count: 0,
            active_child_count: 0,
            active_run_status: state
                .active_run
                .as_ref()
                .and_then(|active| state.runs.get(&active.run_id))
                .map(|run| run.snapshot().status),
        })
    }

    pub(crate) fn permission_scopes(&self) -> Vec<crate::PermissionFileScope> {
        let mut scopes = vec![crate::PermissionFileScope::Global];
        if let Some(workspace_id) = self.environment.workspace_id.clone() {
            scopes.push(crate::PermissionFileScope::Workspace(workspace_id));
        }
        scopes.push(crate::PermissionFileScope::Session(self.id.clone()));
        scopes
    }

    pub(crate) fn reasoning_effort(&self) -> RuntimeResult<Option<ReasoningEffortKey>> {
        Ok(self.lock_state()?.reasoning_effort)
    }

    /// 在持有 Session mutation gate 时撤销当前标题旁路，不留下可恢复 UI 状态。
    pub(crate) fn cancel_title_generation(&self) -> RuntimeResult<()> {
        let mut state = self.lock_state()?;
        if let Some(active) = state.active_title_generation.take() {
            active.cancellation.cancel();
        }
        Ok(())
    }

    pub(crate) fn role(&self) -> RuntimeResult<SessionRole> {
        Ok(self.lock_state()?.role)
    }

    pub(crate) fn ensure_standard_role(&self) -> RuntimeResult<()> {
        if self.role()? == SessionRole::Controller {
            return Err(RuntimeError::SessionRoleRestricted {
                session_id: self.id.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn find_idempotent(
        &self,
        key: &IdempotencyKey,
    ) -> RuntimeResult<Option<(InputId, RunSnapshot)>> {
        let state = self.lock_state()?;
        Ok(state
            .inputs
            .values()
            .find(|input| input.stored.idempotency_key.as_ref() == Some(key))
            .and_then(|input| {
                state
                    .runs
                    .get(&input.first_run_id)
                    .map(|run| (input.stored.input_id.clone(), run.snapshot()))
            }))
    }

    /// 内部状态故障后只允许查询和进程级恢复，不再接受新的 Session 变更。
    pub(crate) fn ensure_healthy(&self) -> RuntimeResult<()> {
        if self.lock_state()?.is_faulted {
            return Err(RuntimeError::SessionFaulted {
                session_id: self.id.clone(),
            });
        }
        Ok(())
    }

    /// 归档 Session 保持可查询，但拒绝全部业务变更。
    pub(crate) fn ensure_active(&self) -> RuntimeResult<()> {
        if self.lock_state()?.lifecycle == SessionLifecycle::Archived {
            return Err(RuntimeError::SessionArchived {
                session_id: self.id.clone(),
            });
        }
        Ok(())
    }

    /// 归档、模型切换和历史重写共用的完全空闲判定。
    pub(crate) fn ensure_idle(&self) -> RuntimeResult<()> {
        let state = self.lock_state()?;
        if state.active_compaction.is_some() {
            return Err(RuntimeError::SessionCompactionInProgress {
                session_id: self.id.clone(),
            });
        }
        let has_nonterminal_run = state.runs.values().any(|run| !run.status().is_terminal());
        if state.active_run.is_some()
            || !state.session_inputs.is_empty()
            || !state.goal_inputs.is_empty()
            || has_nonterminal_run
            || state
                .inputs
                .values()
                .any(|input| input.stored.state == StoredInputState::Queued)
        {
            return Err(RuntimeError::SessionNotIdle {
                session_id: self.id.clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn ensure_not_compacting(&self) -> RuntimeResult<()> {
        if self.lock_state()?.active_compaction.is_some() {
            return Err(RuntimeError::SessionCompactionInProgress {
                session_id: self.id.clone(),
            });
        }
        Ok(())
    }

    /// 让后续业务命令 fail-closed；调用方仍可通过快照观察已经可靠提交的事实。
    pub(crate) fn mark_faulted(&self) -> RuntimeResult<()> {
        let mut state = self.lock_state()?;
        state.is_faulted = true;
        state.is_queue_driver_running = false;
        Ok(())
    }

    pub(crate) fn conversation_snapshot(&self) -> RuntimeResult<ConversationSnapshot> {
        self.lock_state()?
            .journal
            .as_ref()
            .map(InMemoryJournal::snapshot)
            .ok_or_else(|| RuntimeError::StorageUnavailable {
                operation: "load session conversation",
                source: None,
            })
    }

    pub(crate) fn run_snapshot(&self, run_id: &RunId) -> RuntimeResult<RunSnapshot> {
        self.lock_state()?
            .runs
            .get(run_id)
            .map(RunRecord::snapshot)
            .ok_or_else(|| RuntimeError::RunNotFound {
                session_id: self.id.clone(),
                run_id: run_id.clone(),
            })
    }

    pub(crate) fn id(&self) -> &SessionId {
        &self.id
    }

    pub(crate) fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    pub(crate) fn model_key(&self) -> RuntimeResult<ModelKey> {
        Ok(self.lock_state()?.model_key.clone())
    }

    pub(crate) fn system_prompt(&self) -> &SystemPromptSnapshot {
        &self.system_prompt
    }

    pub(crate) fn skill_catalog(&self) -> &SessionSkillCatalog {
        &self.skill_catalog
    }

    pub(crate) fn environment(&self) -> &SessionExecutionEnvironment {
        &self.environment
    }

    pub(crate) fn run_snapshots(&self) -> RuntimeResult<Vec<RunSnapshot>> {
        let state = self.lock_state()?;
        let orders = state
            .inputs
            .values()
            .map(|input| (input.stored.input_id.clone(), input.stored.queue_order))
            .collect::<BTreeMap<_, _>>();
        let mut runs = state
            .runs
            .values()
            .map(|run| {
                (
                    orders.get(run.input_id()).copied().unwrap_or(u64::MAX),
                    run.attempt(),
                    run.snapshot(),
                )
            })
            .collect::<Vec<_>>();
        runs.sort_by(|left, right| {
            (left.0, left.1, left.2.run_id.as_str()).cmp(&(
                right.0,
                right.1,
                right.2.run_id.as_str(),
            ))
        });
        Ok(runs.into_iter().map(|(_, _, run)| run).collect())
    }

    pub(crate) async fn mutation(&self) -> AsyncMutexGuard<'_, ()> {
        self.mutation_gate.lock().await
    }

    pub(crate) async fn ensure_conversation_loaded(
        &self,
        store: &dyn RuntimeStore,
    ) -> RuntimeResult<()> {
        if self.lock_state()?.journal.is_some() {
            return Ok(());
        }
        let _mutation = self.mutation().await;
        {
            let state = self.lock_state()?;
            if state.journal.is_some() {
                return Ok(());
            }
            if !state.is_conversation_available {
                return Err(RuntimeError::StorageUnavailable {
                    operation: "load session conversation",
                    source: None,
                });
            }
        }

        let product_history = match store.load_conversation(&self.id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if error.kind() == crate::StoreErrorKind::InvalidData {
                    self.lock_state()?.is_conversation_available = false;
                }
                return Err(RuntimeError::from_store("load session conversation", error));
            }
        };
        let snapshot = crate::execution_context_from_product_history(&product_history);
        let persisted_message_count = snapshot.messages.len();
        let journal = InMemoryJournal::from_snapshot(snapshot).map_err(|_| {
            RuntimeError::StorageUnavailable {
                operation: "load session conversation",
                source: None,
            }
        })?;
        let mut state = self.lock_state()?;
        for run in state.runs.values_mut() {
            run.hydrate(&product_history);
        }
        state.persisted_message_count = persisted_message_count;
        state.journal = Some(journal);
        Ok(())
    }

    pub(crate) fn lock_state(&self) -> RuntimeResult<MutexGuard<'_, SessionState>> {
        self.state
            .lock()
            .map_err(|_| RuntimeError::InternalStateUnavailable {
                component: "session state",
            })
    }
}

fn map_lifecycle(lifecycle: crate::StoredSessionLifecycle) -> SessionLifecycle {
    match lifecycle {
        crate::StoredSessionLifecycle::Active => SessionLifecycle::Active,
        crate::StoredSessionLifecycle::Archived => SessionLifecycle::Archived,
    }
}
