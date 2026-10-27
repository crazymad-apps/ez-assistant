//! 全局启动与会话摘要不扫描文件；单会话装配按需核对只读文件历史。

use assistant_protocol::{SessionId, SessionSummary};
use assistant_runtime::{LoadedSession, RecoveredRuntime, SessionSummaryQuery};
use rusqlite::params;

use super::{
    StorageEngine, StorageResult, internal_error, invalid_data,
    mode::{parse_agent_variant, parse_approval_mode, parse_reasoning_effort},
    non_negative_u64,
};

impl StorageEngine {
    /// 当前 owner 无待提交尾帧，SQL 已只装配真实队列和控制；将可靠文件事实加入只读恢复投影。
    /// 历史选择补齐已消费 Input 的内存值，不回写 SQL、不注册历史 Run／child，也不发布事件。
    /// 调用者持有 Store worker，失败由所属会话读取报告，不把不完整文件降为缺省值。
    pub(super) fn load_session_history_state(
        &mut self,
        id: &SessionId,
        state: &mut RecoveredRuntime,
    ) -> StorageResult<()> {
        let facts = self
            .load_conversation_history(assistant_protocol::ConversationOwner::MainSession {
                session_id: id.clone(),
            })?
            .facts;
        for activation in &facts.skill_activations {
            if let Some(input_id) = &activation.input_id
                && let Some(input) = state
                    .inputs
                    .iter_mut()
                    .find(|input| &input.session_id == id && &input.input_id == input_id)
            {
                if input.state != assistant_runtime::StoredInputState::Committed
                    || input
                        .skill_activation
                        .as_ref()
                        .is_some_and(|existing| existing != activation)
                {
                    return Err(invalid_data(
                        "historical skill activation conflicts with unfinished input",
                    ));
                }
                input.skill_activation = Some(activation.clone());
            }
        }
        state
            .run_history
            .extend(facts.runs.into_iter().map(|run| (id.clone(), run)));
        state.child_history.extend(
            facts
                .child_tasks
                .into_iter()
                .map(|child| (id.clone(), child)),
        );
        state.skill_activations.extend(facts.skill_activations);
        state.skill_activations.sort_by(|left, right| {
            (left.created_at_ms, &left.activation_id)
                .cmp(&(right.created_at_ms, &right.activation_id))
        });
        state.mcp_input_selections.extend(facts.mcp_selections);
        state.session_commands.extend(facts.commands);
        Ok(())
    }

    pub(super) fn load_runtime_globals(&self) -> StorageResult<RecoveredRuntime> {
        Ok(RecoveredRuntime {
            devices: self.load_devices()?,
            workspaces: self.load_all_workspaces()?,
            ..Default::default()
        })
    }

    pub(super) fn prepare_session_execution(
        &mut self,
        id: &SessionId,
    ) -> StorageResult<LoadedSession> {
        self.repair_session_resources_scoped(Some(id))?;
        self.recover_attachments_scoped(Some(id))?;
        self.recover_session_history_operations_scoped(Some(id))?;
        let mut unavailable = self.recover_body_appends_scoped(Some(id))?;
        let mut children = self.recover_child_body_appends_scoped(Some(id))?;
        children.extend(self.recover_pending_child_tool_exchanges_scoped(Some(id))?);
        self.unavailable_child_tasks.extend(children);
        self.interrupt_nonterminal_child_tasks_scoped(Some(id))?;
        unavailable.extend(self.recover_pending_tool_exchanges_scoped(Some(id))?);
        self.unavailable_sessions.remove(id.as_str());
        self.unavailable_sessions.extend(unavailable);
        // 历史图片只在实际使用时校验；恢复不扫描／解码主窗口之外的图片与已结束 child。
        if !self.unavailable_sessions.contains(id.as_str()) {
            self.pause_running_goals_for_recovery_scoped(Some(id))?;
            self.backfill_session_usage_scoped(Some(id))?;
        }
        let window = self.load_execution_window(id)?;
        super::usage::snapshot::replace_context(
            &self.connection,
            &assistant_protocol::ConversationOwner::MainSession {
                session_id: id.clone(),
            },
            &window.snapshot(),
        )?;
        let mut loaded = self.load_session_state(id)?;
        loaded.execution_window = Some(window);
        Ok(loaded)
    }

    pub(super) fn load_session_state(&mut self, id: &SessionId) -> StorageResult<LoadedSession> {
        let mut state = RecoveredRuntime {
            devices: self.load_devices()?,
            workspaces: self.load_all_workspaces()?,
            sessions: self.load_sessions_scoped(Some(id))?,
            inputs: self.load_inputs_scoped(Some(id))?,
            runs: self.load_unfinished_runs_scoped(Some(id))?,
            session_commands: self.load_session_commands_scoped(id)?,
            mcp_input_selections: self.load_mcp_input_selections_scoped(Some(id))?,
            attachments: self.load_attachments_scoped(Some(id))?,
            child_tasks: self.load_unfinished_child_tasks_scoped(Some(id))?,
            child_history: Vec::new(),
            work_plans: self.load_all_work_plans_scoped(Some(id))?,
            goals: self.load_all_goals_scoped(Some(id))?,
            skill_activations: self.load_skill_activations_scoped(Some(id))?,
            run_history: Vec::new(),
        };
        let pending: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM body_appends WHERE session_id = ?1)
                 OR EXISTS(SELECT 1 FROM pending_tool_exchanges WHERE session_id = ?1)
                 OR EXISTS(SELECT 1 FROM child_body_appends WHERE session_id = ?1)
                 OR EXISTS(SELECT 1 FROM child_pending_tool_exchanges WHERE session_id = ?1)",
                [id.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| internal_error("session pending state could not be queried", e))?;
        if pending {
            for session in &mut state.sessions {
                session.conversation_state =
                    assistant_runtime::StoredConversationState::Unavailable;
            }
        }
        if state.sessions.iter().any(|session| {
            session.conversation_state == assistant_runtime::StoredConversationState::Available
        }) {
            self.load_session_history_state(id, &mut state)?;
        }
        // 身份引用允许跨会话，但不因此装配被引用会话的执行状态。
        let mut ids = std::collections::BTreeSet::new();
        for session in &state.sessions {
            if let Some(proxy) = &session.proxy {
                ids.insert(proxy.controller_session_id.clone());
            }
        }
        for input in &state.inputs {
            if let Some(envelope) = &input.cross_session
                && let assistant_runtime::CrossSessionInputBinding::ControllerDelivery {
                    controller_session_id,
                    ..
                } = &envelope.binding
            {
                ids.insert(controller_session_id.clone());
            }
        }
        let mut identities = Vec::new();
        for reference in ids {
            let rows = self.query_session_summaries(SessionSummaryQuery {
                filter: assistant_protocol::SessionListFilter::All,
                session_id: Some(reference),
                role: None,
                query: None,
                offset: 0,
                limit: 1,
            })?;
            for row in rows {
                identities.push((
                    row.session_id,
                    match row.role {
                        assistant_protocol::SessionRoleSnapshot::Standard => {
                            assistant_runtime::SessionRole::Standard
                        }
                        assistant_protocol::SessionRoleSnapshot::Controller => {
                            assistant_runtime::SessionRole::Controller
                        }
                    },
                    match row.lifecycle {
                        assistant_protocol::SessionLifecycle::Active => {
                            assistant_runtime::StoredSessionLifecycle::Active
                        }
                        assistant_protocol::SessionLifecycle::Archived => {
                            assistant_runtime::StoredSessionLifecycle::Archived
                        }
                    },
                ));
            }
        }
        Ok(LoadedSession {
            state,
            identities,
            execution_window: None,
        })
    }

    pub(super) fn query_session_summaries(
        &self,
        query: SessionSummaryQuery,
    ) -> StorageResult<Vec<SessionSummary>> {
        let lifecycle = match query.filter {
            assistant_protocol::SessionListFilter::All => None,
            assistant_protocol::SessionListFilter::Active => Some("active"),
            assistant_protocol::SessionListFilter::Archived => Some("archived"),
        };
        let role = query.role.map(|role| match role {
            assistant_protocol::SessionRoleSnapshot::Standard => "standard",
            assistant_protocol::SessionRoleSnapshot::Controller => "controller",
        });
        let mut statement = self.connection.prepare(
            "SELECT s.session_id, s.title, json_array(s.model_provider_instance_id, s.model_id), s.reasoning_effort,
                    s.lifecycle, s.role, s.proxy_controller_session_id, s.proxy_changed_at_ms,
                    s.pc_output_device_id, d.display_name, s.current_variant, s.approval_mode,
                    r.workspace_id, s.message_count, s.created_at_ms, s.updated_at_ms,
                    s.archived_at_ms, s.is_pinned, s.title_origin,
                    (SELECT COUNT(*) FROM inputs i WHERE i.session_id = s.session_id
                     AND i.state = 'queued' AND i.input_kind = 'message' AND i.origin = 'user' AND i.goal_id IS NULL),
                    EXISTS(SELECT 1 FROM inputs i WHERE i.session_id = s.session_id AND i.state = 'queued' AND i.goal_id IS NULL),
                    s.custom_agent_json
             FROM sessions s LEFT JOIN devices d ON d.device_id = s.pc_output_device_id
             LEFT JOIN session_resources r ON r.session_id = s.session_id
             WHERE (?1 IS NULL OR s.lifecycle = ?1) AND (?2 IS NULL OR s.session_id = ?2)
               AND (?3 IS NULL OR s.role = ?3) AND (?6 IS NULL OR instr(lower(s.title), lower(?6)) > 0)
             ORDER BY s.is_pinned DESC, s.updated_at_ms DESC, s.session_id ASC LIMIT ?4 OFFSET ?5"
        ).map_err(|e| internal_error("session summaries could not be queried", e))?;
        let mut rows = statement
            .query(params![
                lifecycle,
                query.session_id.as_ref().map(SessionId::as_str),
                role,
                query.limit.max(1),
                query.offset,
                query.query.as_deref()
            ])
            .map_err(|e| internal_error("session summaries could not be read", e))?;
        let mut summaries = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| internal_error("session summary could not be read", e))?
        {
            // 将 SQLite 类型错误留在存储边界，领域枚举逐项严格解码。
            let result = (|| -> Result<_, rusqlite::Error> {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, Option<i64>>(16)?,
                    row.get::<_, bool>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, i64>(19)?,
                    row.get::<_, bool>(20)?,
                    row.get::<_, Option<String>>(21)?,
                ))
            })()
            .map_err(|e| internal_error("session summary row is invalid", e))?;
            let (
                id,
                title,
                model,
                effort,
                lifecycle,
                role,
                proxy,
                changed,
                device,
                device_name,
                variant,
                approval,
                workspace,
                count,
                created,
                updated,
                archived,
                pinned,
                title_origin,
                queued,
                resume_required,
                custom_agent_json,
            ) = result;
            let queued_input_count = non_negative_u64(queued, "invalid queued input count")?;
            summaries.push(SessionSummary {
                session_id: SessionId::new(id).map_err(|_| invalid_data("invalid session id"))?,
                custom_agent: custom_agent_json
                    .map(|value| {
                        serde_json::from_str::<assistant_protocol::CustomAgentSnapshot>(&value)
                    })
                    .transpose()
                    .map_err(|_| invalid_data("invalid custom agent snapshot"))?
                    .map(|agent| assistant_protocol::CustomAgentSelection {
                        name: agent.config.name,
                        source: agent.source,
                    }),
                title,
                model_selection: super::model_management::read_selection_json(&model)?,
                reasoning_effort: parse_reasoning_effort(effort)?,
                lifecycle: match lifecycle.as_str() {
                    "active" => assistant_protocol::SessionLifecycle::Active,
                    "archived" => assistant_protocol::SessionLifecycle::Archived,
                    _ => return Err(invalid_data("invalid session lifecycle")),
                },
                role: match role.as_str() {
                    "standard" => assistant_protocol::SessionRoleSnapshot::Standard,
                    "controller" => assistant_protocol::SessionRoleSnapshot::Controller,
                    _ => return Err(invalid_data("invalid session role")),
                },
                proxy: match (proxy, changed) {
                    (Some(id), Some(changed_at_ms)) => {
                        Some(assistant_protocol::SessionProxySnapshot {
                            controller_session_id: SessionId::new(id)
                                .map_err(|_| invalid_data("invalid controller id"))?,
                            changed_at_ms,
                        })
                    }
                    (None, None) => None,
                    _ => return Err(invalid_data("incomplete proxy")),
                },
                pc_output_hosting: match (device, device_name) {
                    (Some(id), Some(device_name)) => {
                        Some(assistant_protocol::PcOutputHostingSnapshot {
                            device_id: assistant_protocol::DeviceId::new(id)
                                .map_err(|_| invalid_data("invalid device id"))?,
                            device_name,
                        })
                    }
                    (None, None) => None,
                    _ => return Err(invalid_data("incomplete hosting")),
                },
                current_variant: parse_agent_variant(&variant)?,
                approval_mode: parse_approval_mode(&approval)?,
                workspace_id: workspace
                    .map(assistant_protocol::WorkspaceId::new)
                    .transpose()
                    .map_err(|_| invalid_data("invalid workspace id"))?,
                message_count: non_negative_u64(count, "invalid message count")?,
                queued_input_count,
                resume_required,
                created_at_ms: Some(created),
                updated_at_ms: Some(updated),
                archived_at_ms: archived,
                is_pinned: pinned,
                title_origin: match title_origin.as_str() {
                    "user" => assistant_protocol::SessionTitleOrigin::User,
                    "generated" => assistant_protocol::SessionTitleOrigin::Generated,
                    _ => return Err(invalid_data("invalid title origin")),
                },
                active_compaction: None,
                active_run_id: None,
                pending_approval_count: 0,
                active_child_count: 0,
                active_run_status: None,
            });
        }
        Ok(summaries)
    }
}
