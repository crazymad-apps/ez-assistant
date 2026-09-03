//! 与普通消息共享 FIFO、但不创建 Run 的 Session 控制指令。

use agent_types::ConversationMessage;
use assistant_protocol::{
    AcceptedSessionCommand, ConversationOwner, McpDiagnosticCode, McpDiagnosticSnapshot,
    McpRefreshControlResultSnapshot, McpRefreshOutcome, McpServerRefreshOutcome,
    McpServerRefreshResultSnapshot, RuntimeEvent, SessionCommand, SubmitSessionCommandRequest,
    SubmitSessionCommandResult,
};

use super::{AssistantRuntime, QueueDriverContext};
use crate::{
    NewStoredSessionCommand, RuntimeError, RuntimeResult, SessionCommandCommit,
    StoredSessionCommand,
    internal_boundary::{
        InternalBoundaryCoordinator, InternalBoundaryRequest, InternalBoundarySource,
    },
    session::SessionController,
};

const CONTROL_RESULT_MARKER: &str = "{RUNTIME_CONTROL_RESULT_V1}";
const MAX_MODEL_RESULT_SERVERS: usize = 64;
const MAX_MODEL_RESULT_BYTES: usize = 16 * 1024;

impl AssistantRuntime {
    /// 先可靠接纳结构化指令，再发布 Queue 快照失效；不把 slash 文本当作用户输入。
    pub async fn submit_session_command(
        &self,
        request: SubmitSessionCommandRequest,
    ) -> RuntimeResult<SubmitSessionCommandResult> {
        let _operation = self.operation_gate.read().await;
        self.ensure_running()?;
        self.mcp_service.ensure_available()?;
        let session = self.session(&request.session_id)?;
        let _mutation = session.mutation().await;
        session.ensure_active()?;
        session.ensure_healthy()?;
        session.ensure_not_compacting()?;
        let (input_id, agent_variant) = {
            let state = session.lock_state()?;
            if state.role != crate::SessionRole::Standard {
                return Err(RuntimeError::InvalidRequest {
                    reason: "session commands require a standard session",
                });
            }
            if let Some(key) = request.idempotency_key.as_ref()
                && let Some(existing) = state
                    .commands
                    .values()
                    .find(|command| command.idempotency_key.as_ref() == Some(key))
            {
                if existing.command != request.command {
                    return Err(RuntimeError::InvalidRequest {
                        reason: "session command idempotency key was reused with different content",
                    });
                }
                return Ok(SubmitSessionCommandResult {
                    accepted: AcceptedSessionCommand {
                        input_id: existing.input_id.clone(),
                        command: existing.command.clone(),
                        is_duplicate: true,
                    },
                });
            }
            (self.allocate_input_id(&state)?, state.current_variant)
        };
        let accepted_at_ms = super::super::now_ms()?;
        let accepted = self
            .store
            .accept_session_command(NewStoredSessionCommand {
                input_id,
                session_id: request.session_id.clone(),
                idempotency_key: request.idempotency_key,
                user_message_id: crate::run::allocate_message_id()?,
                agent_variant,
                command: request.command,
                accepted_at_ms,
            })
            .await
            .map_err(|source| RuntimeError::from_store("accept session command", source))?;
        let result = SubmitSessionCommandResult {
            accepted: AcceptedSessionCommand {
                input_id: accepted.command.input_id.clone(),
                command: accepted.command.command.clone(),
                is_duplicate: accepted.is_duplicate,
            },
        };
        if !accepted.is_duplicate {
            let revision = {
                let mut state = session.lock_state()?;
                let input_id = accepted.command.input_id.clone();
                state.commands.insert(input_id.clone(), accepted.command);
                state.queue_item_ids.push_back(input_id);
                state.queue_revision = state.queue_revision.saturating_add(1);
                state.updated_at_ms = accepted_at_ms;
                if state.goal.is_some() {
                    state.resume_required = true;
                }
                state.queue_revision
            };
            self.publish(RuntimeEvent::QueueChanged {
                session_id: request.session_id.clone(),
                revision,
            });
            self.publish(RuntimeEvent::SessionChanged {
                session_id: request.session_id,
            });
            self.wake_queue(session.clone())?;
        }
        Ok(result)
    }
}

/// 调用方已占用唯一 Queue driver 消费位；网络等待不阻塞新消息的可靠接纳。
pub(super) async fn execute_session_command(
    context: &QueueDriverContext,
    session: &SessionController,
    command: StoredSessionCommand,
) -> RuntimeResult<()> {
    let SessionCommand::McpRefresh { server } = &command.command;
    let result = match super::super::mcp::refresh_mcp_registry_with(
        context.mcp_config_store.as_ref(),
        context.mcp_registry.as_ref(),
        context.config_registry.as_ref(),
        context.approval_registry.as_ref(),
        &context.events,
        &context.root_cancellation,
        server.as_ref(),
    )
    .await
    {
        Ok(result) => result,
        Err(RuntimeError::ConfigurationUnavailable | RuntimeError::McpConfigInvalid) => {
            configuration_refresh_failure(context, server.as_ref())?
        }
        Err(error) => return Err(error),
    };
    let mut message = InternalBoundaryCoordinator::visible_message(InternalBoundaryRequest {
        source: InternalBoundarySource::McpRefreshResult,
        text: render_model_result(&result)?,
    })?
    .0;
    message.id = command.user_message_id.clone();
    let _mutation = session.mutation().await;
    let committed_at_ms = super::super::now_ms()?;
    let committed = context
        .store
        .commit_session_command(SessionCommandCommit {
            operation_id: crate::id::generate("command-commit").map_err(|_| {
                RuntimeError::InternalStateUnavailable {
                    component: "session command operation id",
                }
            })?,
            input_id: command.input_id.clone(),
            session_id: session.id().clone(),
            result,
            message: message.clone(),
            committed_at_ms,
        })
        .await
        .map_err(|source| RuntimeError::from_store("commit session command", source))?;
    let (revision, generation) =
        {
            let mut state = session.lock_state()?;
            if state.executing_command.as_ref() != Some(&command.input_id)
                || state.pop_runnable_input(&command.input_id) != Some(true)
            {
                return Err(RuntimeError::InternalStateUnavailable {
                    component: "committed session command queue projection",
                });
            }
            state
                .journal
                .as_mut()
                .ok_or(RuntimeError::InternalStateUnavailable {
                    component: "session command conversation journal",
                })?
                .append_completed(ConversationMessage::User(message))
                .map_err(|_| RuntimeError::InternalStateUnavailable {
                    component: "session command conversation projection",
                })?;
            state.persisted_message_count += 1;
            state.message_count += 1;
            state.body_generation = state.body_generation.checked_add(1).ok_or(
                RuntimeError::InternalStateUnavailable {
                    component: "session command conversation generation",
                },
            )?;
            state.commands.insert(command.input_id, committed);
            state.executing_command = None;
            state.queue_revision = state.queue_revision.saturating_add(1);
            state.updated_at_ms = committed_at_ms;
            (state.queue_revision, state.body_generation)
        };
    let _ = context.events.send(RuntimeEvent::QueueChanged {
        session_id: session.id().clone(),
        revision,
    });
    let _ = context.events.send(RuntimeEvent::ConversationCommitted {
        owner: ConversationOwner::MainSession {
            session_id: session.id().clone(),
        },
        generation,
    });
    let _ = context.events.send(RuntimeEvent::SessionChanged {
        session_id: session.id().clone(),
    });
    Ok(())
}

fn configuration_refresh_failure(
    context: &QueueDriverContext,
    target: Option<&assistant_protocol::McpServerKey>,
) -> RuntimeResult<McpRefreshControlResultSnapshot> {
    let servers = target
        .map(|server_key| {
            let tool_count = context
                .mcp_registry
                .catalog_server(server_key)?
                .map_or(0, |server| {
                    u32::try_from(server.tools.len()).unwrap_or(u32::MAX)
                });
            Ok(McpServerRefreshResultSnapshot {
                server_key: server_key.clone(),
                outcome: McpServerRefreshOutcome::RetainedAfterFailure,
                tool_count,
                diagnostic: Some(McpDiagnosticSnapshot {
                    server_key: Some(server_key.clone()),
                    code: McpDiagnosticCode::InvalidConfig,
                    field_path: None,
                    message:
                        "MCP configuration could not be loaded; existing connections were retained"
                            .to_owned(),
                }),
            })
        })
        .transpose()?
        .into_iter()
        .collect();
    Ok(McpRefreshControlResultSnapshot {
        outcome: McpRefreshOutcome::Failure,
        servers,
    })
}

fn render_model_result(result: &McpRefreshControlResultSnapshot) -> RuntimeResult<String> {
    // 模型只需要稳定结果，不接收 endpoint、进程参数、secret、原始错误或目录 diff。
    let servers = result
        .servers
        .iter()
        .take(MAX_MODEL_RESULT_SERVERS)
        .map(|server| {
            serde_json::json!({
                "server": server.server_key,
                "outcome": server.outcome,
                "tool_count": server.tool_count,
                "diagnostic": server.diagnostic.as_ref().map(|diagnostic| diagnostic.code),
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "command": "mcp_refresh",
        "outcome": result.outcome,
        "servers": servers,
        "omitted_servers": result.servers.len().saturating_sub(MAX_MODEL_RESULT_SERVERS),
        "guidance": "Use the current MCP directory and discover tools again before calling them. If refresh failed, inspect MCP settings and retry the command.",
    });
    let text = format!("{CONTROL_RESULT_MARKER}\n{payload}");
    if text.len() > MAX_MODEL_RESULT_BYTES {
        return Err(RuntimeError::InternalStateUnavailable {
            component: "session command model result bound",
        });
    }
    Ok(text)
}
