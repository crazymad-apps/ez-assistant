//! 普通与 Goal 用户输入共享的准备、原子接受和 Session 投影。
//!
//! 本模块把 Desktop 与受信任 Channel 输入归一到同一个 Session Input 用例。规范 UserMessage、
//! Input、首次 Run、可选 Goal/Skill 事实先由 Store 原子接受，成功后才更新 Session 内存投影、
//! 发布观察事件并唤醒执行队列；Channel 不会因此形成第二套会话或消息存储。

use assistant_protocol::{
    IdempotencyKey, SessionTitleOrigin, SubmitInputMode, SubmitInputRequest, SubmitInputResult,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use super::super::{
    AssistantRuntime,
    goal::{GoalSubmissionPersistence, PreparedGoalSubmission},
};
use crate::{
    InputChannelSource, InputOrigin, NewStoredInput, RuntimeError, RuntimeResult,
    SkillActivationOwner, SkillActivationResolveError, SkillActivationTrigger, SkillName,
    StoredMcpSelection, StoredSkillActivation, SubmitSessionInputRequest, id,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    run::{RunRecord, allocate_run_id, create_user_message},
    skill::render_user_activation,
};

use super::projection::project_accepted_input;
use crate::runtime::quote::insert_quotes;

impl AssistantRuntime {
    /// 提交一条由 Host 渠道适配器识别来源的 Session 输入。
    ///
    /// Host 负责选择产品路由目标；Runtime 不把 Controller 角色写入通用输入用例。设备来源仍在
    /// 可靠接受前复核 paired 身份，并把客户端输入身份映射到既有幂等键。
    ///
    /// # Errors
    ///
    /// 来源授权已失效，或统一输入接受链失败时返回错误。
    pub async fn submit_session_input(
        &self,
        request: SubmitSessionInputRequest,
    ) -> RuntimeResult<SubmitInputResult> {
        let mut input = request.input;
        if let InputChannelSource::Device(source) = &request.source {
            if self.paired_device(&source.device_id)?.is_none() {
                return Err(RuntimeError::InvalidRequest {
                    reason: "device source is not authorized",
                });
            }
            if input.mode != SubmitInputMode::Normal
                || !input.attachment_ids.is_empty()
                || !input.quotes.is_empty()
                || input.skill_name.is_some()
                || input.mcp_server_key.is_some()
            {
                return Err(RuntimeError::InvalidRequest {
                    reason: "device input contains unsupported product intent",
                });
            }
            input.idempotency_key = Some(device_input_idempotency_key(source)?);
        }
        self.accept_session_input(input, request.source).await
    }

    /// 单元测试使用的 Desktop 输入便利入口；产品 Host 只调用统一 Session 输入用例。
    #[cfg(test)]
    pub(crate) async fn submit_input(
        &self,
        request: SubmitInputRequest,
    ) -> RuntimeResult<SubmitInputResult> {
        self.submit_session_input(SubmitSessionInputRequest {
            input: request,
            source: InputChannelSource::desktop_text(),
        })
        .await
    }

    /// 完成一次 Session Input 的可靠接受与内存投影。
    ///
    /// 调用期间持有 Runtime 操作/模型绑定读 gate 和目标 Session mutation gate，保证配置绑定与
    /// 同 Session 写入顺序稳定；短期 Session 状态锁不会跨越 Store `await`。Store 成功前不修改
    /// Session 投影、不发布事件、不唤醒队列。可靠事实已经提交但后续投影失败时不回滚 Store，
    /// 调用返回错误，后续可从 Store 权威事实恢复。
    async fn accept_session_input(
        &self,
        request: SubmitInputRequest,
        channel_source: InputChannelSource,
    ) -> RuntimeResult<SubmitInputResult> {
        let _operation = self.operation_gate.read().await;
        let _binding = self.model_binding_gate.read().await;
        self.ensure_running()?;
        if request.message.trim().is_empty()
            && request.attachment_ids.is_empty()
            && request.quotes.is_empty()
        {
            return Err(RuntimeError::InvalidRequest {
                reason: "input must contain text, attachments, or quotes",
            });
        }
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        // 先命中 Session 内已恢复的幂等事实，使合法重试不受当前配置、附件或 Skill 变化影响。
        if let Some(key) = request.idempotency_key.as_ref()
            && let Some((input_id, run)) = session.find_idempotent(key)?
        {
            return Ok(SubmitInputResult { input_id, run });
        }
        session.ensure_not_compacting()?;
        session.ensure_healthy()?;
        let quotes = self
            .normalize_quotes(&request.session_id, &request.quotes)
            .await?;
        let model_key = session.model_key()?;
        let configuration = self.config_registry.snapshot()?;
        if configuration
            .active()
            .and_then(|active| active.model(&model_key))
            .is_none()
        {
            return Err(RuntimeError::ModelUnavailable { model_key });
        }
        let selected_skill = request
            .skill_name
            .as_ref()
            .map(|name| SkillName::parse(name.clone()).map_err(|_| RuntimeError::SkillNameInvalid))
            .transpose()?;
        let selected_mcp = request
            .mcp_server_key
            .as_ref()
            .map(|server_key| {
                let server = self
                    .mcp_service
                    .registry
                    .catalog_server(server_key)?
                    .ok_or(RuntimeError::McpServerUnavailable)?;
                let visible = server.tools.iter().any(|tool| {
                    self.permission_coordinator
                        .mcp_tool_is_explicitly_denied(
                            &session.permission_scopes(),
                            request.variant,
                            server_key,
                            &tool.name,
                        )
                        .is_ok_and(|denied| !denied)
                });
                if !visible {
                    return Err(RuntimeError::McpServerUnavailable);
                }
                Ok((server_key.clone(), server.display_name))
            })
            .transpose()?;
        let goal_submission = self.goal_submission(session.as_ref(), request.mode)?;
        let generated_title = automatic_session_title(&request.message);
        let files = self.resolve_file_references(&request.session_id, &request.attachment_ids)?;
        let mut message = create_user_message(request.message, files, request.variant)?;
        insert_quotes(&mut message, &quotes)?;
        // 结构化 channel_source 才是授权和回复路由事实；此 InternalContext 只提示模型本轮交互形态。
        if let Some(text) = channel_input_context(&channel_source) {
            InternalBoundaryCoordinator::insert_before(
                &mut message,
                InternalBoundarySource::AgentVariant,
                InternalBoundaryRequest {
                    source: InternalBoundarySource::ChannelInput,
                    text,
                },
            )?;
        }
        let (input_id, run_id, approval_mode, should_generate_title) = {
            let state = session.lock_state()?;
            (
                self.allocate_input_id(&state)?,
                allocate_run_id(&state)?,
                state.approval_mode,
                state.title_origin == SessionTitleOrigin::Generated
                    && state.inputs.is_empty()
                    && state.message_count == 0,
            )
        };
        let accepted_at_ms = super::super::now_ms()?;
        let mut prepared_goal = goal_submission.prepare(&mut message, accepted_at_ms)?;
        if let Some(goal) = prepared_goal.as_mut()
            && let Some((server_key, _)) = selected_mcp.as_ref()
        {
            goal.control.mcp_server_key = Some(server_key.clone());
        }
        let mcp_selection = selected_mcp
            .map(|(server_key, display_name)| {
                Ok(StoredMcpSelection {
                    selection_id: id::generate("mcp-selection").map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "MCP selection id random source",
                        }
                    })?,
                    session_id: session.id().clone(),
                    input_id: Some(input_id.clone()),
                    message_id: message.id.clone(),
                    server_key,
                    display_name,
                    created_at_ms: accepted_at_ms,
                })
            })
            .transpose()?;
        let skill_activation = selected_skill
            .as_ref()
            .map(|name| {
                let definition =
                    session
                        .skill_catalog()
                        .user_definition(name)
                        .map_err(|error| match error {
                            SkillActivationResolveError::CatalogUnavailable => {
                                RuntimeError::SkillCatalogUnavailable {
                                    session_id: session.id().clone(),
                                }
                            }
                            SkillActivationResolveError::NotFound => RuntimeError::SkillNotFound {
                                session_id: session.id().clone(),
                            },
                            SkillActivationResolveError::NotUserInvocable => {
                                RuntimeError::SkillNotUserInvocable {
                                    session_id: session.id().clone(),
                                }
                            }
                        })?;
                InternalBoundaryCoordinator::append(
                    &mut message,
                    InternalBoundaryRequest {
                        source: InternalBoundarySource::SkillActivation,
                        text: render_user_activation(&session.skill_catalog().revision, definition),
                    },
                )?;
                Ok(StoredSkillActivation {
                    activation_id: id::generate("skill-activation").map_err(|_| {
                        RuntimeError::InternalStateUnavailable {
                            component: "skill activation id random source",
                        }
                    })?,
                    session_id: session.id().clone(),
                    owner: SkillActivationOwner::Session(session.id().clone()),
                    run_id: Some(run_id.clone()),
                    input_id: Some(input_id.clone()),
                    message_id: message.id.clone(),
                    name: definition.name.clone(),
                    catalog_revision: session.skill_catalog().revision.clone(),
                    definition_digest: definition.definition_digest.clone(),
                    trigger: SkillActivationTrigger::User,
                    created_at_ms: accepted_at_ms,
                })
            })
            .transpose()?;
        let goal_binding = prepared_goal.as_ref().map(|goal| goal.binding.clone());
        let new_goal = prepared_goal
            .as_ref()
            .filter(|goal| matches!(goal.persistence, GoalSubmissionPersistence::Start))
            .map(|goal| goal.control.to_stored(session.id().clone()));
        let resumed_goal = prepared_goal
            .as_ref()
            .filter(|goal| matches!(goal.persistence, GoalSubmissionPersistence::Resume))
            .map(|goal| goal.control.to_stored(session.id().clone()));
        let generated_title = should_generate_title.then_some(generated_title);
        let was_proxied = session.lock_state()?.proxy.is_some();
        // Store 在一个业务操作中提交正文、Input、首次 Run，以及可选 Goal/Skill/标题事实；
        // 对普通用户输入，它还会原子解除 proxy 并移除尚未领取的 ControllerDelivery。
        let accepted = self
            .store
            .accept_input(NewStoredInput {
                input_id: input_id.clone(),
                run_id: run_id.clone(),
                session_id: session.id().clone(),
                idempotency_key: request.idempotency_key,
                agent_variant: request.variant,
                origin: InputOrigin::User,
                goal_binding: goal_binding.clone(),
                cross_session: None,
                channel_source: Some(channel_source),
                skill_activation: skill_activation.clone(),
                mcp_selection: mcp_selection.clone(),
                approval_mode,
                message,
                new_goal,
                resumed_goal,
                generated_title: generated_title.clone(),
                accepted_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("accept input", source))?;
        if accepted.is_duplicate {
            // Store 级幂等是跨进程重启的最终防线；重复请求不得再次附加消息或修改队列投影。
            let state = session.lock_state()?;
            let run = state
                .runs
                .get(&accepted.run.run_id)
                .map(RunRecord::snapshot)
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "idempotent run projection",
                })?;
            return Ok(SubmitInputResult {
                input_id: accepted.input.input_id,
                run,
            });
        }
        let goal_snapshot = prepared_goal
            .as_ref()
            .map(|prepared| super::super::product::project_goal(&prepared.control))
            .transpose()?;
        // 到这里可靠事实已经完成；以下只把 Store 返回结果及其伴随效果镜像到当前 Session 投影。
        let (projection, queue_revision) =
            {
                let mut state = session.lock_state()?;
                // accept_input 已在 Store 内完成相同删除，这里不能保留一份与持久事实不一致的旧队列。
                let removed_deliveries =
                    state
                        .inputs
                        .values()
                        .filter(|candidate| {
                            candidate.stored.state == crate::StoredInputState::Queued
                        && candidate.stored.cross_session.as_ref().is_some_and(|envelope| {
                            matches!(
                                envelope.binding,
                                crate::CrossSessionInputBinding::ControllerDelivery { .. }
                            )
                        })
                        })
                        .map(|candidate| candidate.stored.input_id.clone())
                        .collect::<std::collections::BTreeSet<_>>();
                if was_proxied {
                    state.proxy = None;
                }
                if !removed_deliveries.is_empty() {
                    state
                        .queue_item_ids
                        .retain(|input_id| !removed_deliveries.contains(input_id));
                    state
                        .inputs
                        .retain(|input_id, _| !removed_deliveries.contains(input_id));
                    state
                        .runs
                        .retain(|_, run| !removed_deliveries.contains(run.input_id()));
                }
                if let Some(title) = generated_title {
                    state.title = title;
                }
                let changes_session_queue = accepted.input.goal_binding.is_none();
                if let Some(PreparedGoalSubmission { control, .. }) = prepared_goal {
                    state.goal = Some(control);
                }
                state.queue_paused_by_user = false;
                let projection = project_accepted_input(&mut state, accepted, mcp_selection);
                if !changes_session_queue && !removed_deliveries.is_empty() {
                    state.queue_revision = state.queue_revision.saturating_add(1);
                }
                let queue_revision = (changes_session_queue || !removed_deliveries.is_empty())
                    .then_some(state.queue_revision);
                (projection, queue_revision)
            };
        self.publish(assistant_protocol::RuntimeEvent::RunAccepted {
            session_id: session.id().clone(),
            run_id: projection.run.run_id.clone(),
        });
        if let Some(goal) = goal_snapshot {
            self.publish(assistant_protocol::RuntimeEvent::GoalChanged {
                session_id: session.id().clone(),
                goal_id: goal.goal_id,
                generation: goal.generation,
            });
        }
        if was_proxied {
            self.publish(assistant_protocol::RuntimeEvent::SessionChanged {
                session_id: session.id().clone(),
            });
        }
        if let Some(revision) = queue_revision {
            self.publish(assistant_protocol::RuntimeEvent::QueueChanged {
                session_id: session.id().clone(),
                revision,
            });
        }
        self.wake_queue(session.clone())?;
        Ok(SubmitInputResult {
            input_id: projection.input_id,
            run: projection.run,
        })
    }
}

/// 将设备侧输入身份收敛到 Runtime 既有幂等键空间。
///
/// 命名空间版本、稳定 Device ID 与设备内 `client_input_id` 一起参与摘要，因此不同设备可以安全
/// 复用自己的编号；幂等索引不直接采用外部字符串，也不会把设备编号误当成 Runtime Message ID。
fn device_input_idempotency_key(
    source: &crate::DeviceInputSource,
) -> RuntimeResult<IdempotencyKey> {
    if source.client_input_id.trim().is_empty() || source.client_input_id.len() > 256 {
        return Err(RuntimeError::InvalidRequest {
            reason: "device client input id is invalid",
        });
    }
    let mut digest = Sha256::new();
    digest.update(b"device-input-v1\0");
    digest.update(source.device_id.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(source.client_input_id.as_bytes());
    IdempotencyKey::new(format!(
        "channel:device:{}",
        URL_SAFE_NO_PAD.encode(digest.finalize())
    ))
    .map_err(|_| RuntimeError::InternalStateUnavailable {
        component: "device input idempotency key",
    })
}

/// 生成仅供模型理解本轮渠道语义的内部上下文。
///
/// 设备身份、模态和输出偏好的权威事实仍是 `InputChannelSource`；此文本不参与授权、幂等或投递
/// 解析。Desktop 输入返回 `None`，保持既有模型上下文完全不变。
fn channel_input_context(source: &InputChannelSource) -> Option<String> {
    let InputChannelSource::Device(source) = source else {
        return None;
    };
    let modality = match source.modality {
        crate::InputModality::Text => "text",
        crate::InputModality::SpeechTranscript => "speech_transcript",
    };
    let (reply_preference, reply_instruction) = match source.requested_output {
        crate::OutputPreference::Text => (
            "text",
            "Respond normally in text. Do not call the speak tool solely for channel delivery.",
        ),
        crate::OutputPreference::Audio => (
            "audio",
            "Call the speak tool with a concise, natural spoken reply for this turn before final completion.",
        ),
        crate::OutputPreference::TextAndAudio => (
            "text_and_audio",
            "Call the speak tool with a concise, natural spoken reply for this turn before final completion.",
        ),
    };
    Some(format!(
        "<channel_input>\nsource: intelligent_terminal\ninput_modality: {modality}\nreply_preference: {reply_preference}\nreply_instruction: {reply_instruction}\n</channel_input>"
    ))
}

pub(in crate::runtime) fn automatic_session_title(message: &str) -> String {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("New Session");
    line.chars()
        .take(AssistantRuntime::MAX_SESSION_TITLE_CHARS)
        .collect()
}
