//! 首次执行准备闭合旧进程的 pending 控制；只复用可靠提交，不访问模型或执行历史工具。

use std::time::{SystemTime, UNIX_EPOCH};

use agent_core::ExchangeReceipt;
use agent_types::{ToolCall, ToolMessage, ToolResult, ToolResultContent, ToolResultStatus};

use super::*;

impl VolatileRuntimeStore {
    /// 调用方是当前 Session 的唯一执行准备者，尚未启动该 Session 的新执行。
    /// 每次只快照本 Session 的未完成控制，释放状态锁后调用既有完整提交入口；不跨 await
    /// 持锁、不建立恢复日志、不恢复执行对象。顺序为 child 交换、child 终态、父交换；父 Run
    /// 仍由既有 Runtime 准备流程结算。失败保留尚未闭合的原控制，已完成提交可幂等继续。
    pub(super) async fn recover_interrupted_exchanges(
        &self,
        session_id: &SessionId,
    ) -> Result<(), StoreError> {
        if self
            .lock()?
            .sessions
            .get(session_id)
            .is_some_and(|session| {
                session.conversation_state == StoredConversationState::Unavailable
            })
        {
            return Ok(());
        }
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .ok_or_else(|| {
                StoreError::new(StoreErrorKind::Internal, "recovery time is unavailable")
            })?;
        let children = {
            let state = self.lock()?;
            state
                .pending_child_tool_exchanges
                .iter()
                .filter(|(_, pending)| &pending.session_id == session_id)
                .map(|(receipt, pending)| (receipt.clone(), pending.clone()))
                .collect::<Vec<_>>()
        };
        for (receipt, pending) in children {
            let results = tool_calls(&pending.assistant)
                .enumerate()
                .map(|(index, call)| {
                    unknown_result(
                        &receipt,
                        index,
                        call,
                        pending.started_calls.contains(call.id.as_str()),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.complete_child_tool_exchange(CompletedChildToolExchange {
                operation_id: format!("recover-child-exchange:{receipt}"),
                receipt: ExchangeReceipt::new(receipt)
                    .map_err(|_| invalid("child recovery receipt is invalid"))?,
                child_task_id: pending.child_task_id,
                session_id: pending.session_id,
                step: pending.step,
                results,
                activation_message: None,
                skill_activations: Vec::new(),
                completed_at_ms: at_ms,
            })
            .await?;
        }
        let children = {
            let state = self.lock()?;
            state
                .child_tasks
                .values()
                .filter(|task| {
                    &task.session_id == session_id
                        && !task.status.is_terminal()
                        && task.conversation_state == StoredConversationState::Available
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        for child in children {
            self.settle_child_task(StoredChildTaskSettlement {
                operation_id: format!("recover-child:{}", child.child_task_id),
                child_task_id: child.child_task_id,
                session_id: child.session_id,
                status: ChildTaskStatus::Interrupted,
                cancel_requested: child.cancel_requested,
                error: child.error,
                messages: Vec::new(),
                final_message_id: child.final_message_id,
                finished_at_ms: at_ms,
            })
            .await?;
        }
        let parents = {
            let state = self.lock()?;
            state
                .pending_tool_exchanges
                .iter()
                .filter(|(_, pending)| &pending.session_id == session_id)
                .map(|(receipt, pending)| (receipt.clone(), pending.clone()))
                .collect::<Vec<_>>()
        };
        for (receipt, pending) in parents {
            let results = {
                let state = self.lock()?;
                tool_calls(&pending.assistant)
                    .enumerate()
                    .map(|(index, call)| {
                        if call.name.as_str() == crate::DELEGATE_TASK_TOOL_NAME
                            && pending.started_calls.contains(call.id.as_str())
                        {
                            delegate_result(&state, &pending, &receipt, index, call)
                        } else {
                            unknown_result(
                                &receipt,
                                index,
                                call,
                                pending.started_calls.contains(call.id.as_str()),
                            )
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            self.complete_tool_exchange(CompletedToolExchange {
                operation_id: format!("recover-parent-exchange:{receipt}"),
                receipt: ExchangeReceipt::new(receipt)
                    .map_err(|_| invalid("parent recovery receipt is invalid"))?,
                session_id: pending.session_id,
                run_id: pending.run_id,
                step: pending.step,
                results,
                activation_message: None,
                skill_activations: Vec::new(),
                completed_at_ms: at_ms,
            })
            .await?;
        }
        Ok(())
    }
}

fn tool_calls(assistant: &AssistantMessage) -> impl Iterator<Item = &ToolCall> {
    assistant.parts.iter().filter_map(|part| match part {
        AssistantPart::ToolCall(call) => Some(call),
        _ => None,
    })
}

fn unknown_result(
    receipt: &str,
    index: usize,
    call: &ToolCall,
    started: bool,
) -> Result<ToolMessage, StoreError> {
    Ok(ToolMessage {
        id: recovered_message_id(receipt, index)?,
        result: ToolResult {
            call_id: call.id.clone(),
            status: ToolResultStatus::Error,
            content: ToolResultContent::text(if started {
                "runtime restarted; tool execution outcome is unknown"
            } else {
                "runtime restarted before tool execution started"
            }),
            metadata: None,
        },
    })
}

/// child 已可靠结算且控制已释放；只读自身历史与正文恢复原委派结果，不推测完成事实。
/// 调用方仅持 Store 状态锁；本函数不写状态、不发布事件，缺失/冲突证据必须保留父 pending。
fn delegate_result(
    state: &State,
    pending: &VolatilePendingExchange,
    receipt: &str,
    index: usize,
    call: &ToolCall,
) -> Result<ToolMessage, StoreError> {
    let mut bindings = state
        .histories
        .get(&pending.session_id)
        .into_iter()
        .flat_map(|history| &history.child_tasks)
        .filter(|child| {
            child.parent_run_id == pending.run_id
                && child.parent_tool_call_id.as_str() == call.id.as_str()
        });
    let binding = bindings.next();
    if bindings.next().is_some() {
        return Err(invalid("delegated child history is ambiguous"));
    }
    let content;
    let status;
    if let Some(binding) = binding {
        let child = state
            .child_history(&pending.session_id, &binding.child_task_id)
            .ok_or_else(|| invalid("delegated child history is missing"))?;
        if binding.inherited
            || child.inherited
            || child.parent_run_id != pending.run_id
            || child.parent_tool_call_id != binding.parent_tool_call_id
            || child.body_generation != binding.body_generation
        {
            return Err(invalid("delegated child history binding is invalid"));
        }
        let terminal = child
            .status
            .filter(|status| status.is_terminal())
            .ok_or_else(|| invalid("delegated child terminal fact is missing"))?;
        if terminal == ChildTaskStatus::Completed {
            let final_id = child
                .final_message_id
                .as_ref()
                .ok_or_else(|| invalid("completed child final identity is missing"))?;
            let message = state
                .child_conversations
                .get(&(pending.session_id.clone(), child.child_task_id.clone()))
                .and_then(|body| {
                    body.messages
                        .iter()
                        .find(|message| message_id(message) == final_id)
                });
            let Some(ConversationMessage::Assistant(message)) = message else {
                return Err(invalid("completed child final message is missing"));
            };
            let result = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    AssistantPart::Text(part) => Some(part.text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            content = ToolResultContent::json(
                serde_json::json!({"task_id":child.child_task_id,"status":"completed","result":result}),
            );
            status = ToolResultStatus::Success;
        } else {
            let code = match &child.error {
                Some(error) => serde_json::to_value(error.code),
                None => serde_json::to_value(terminal),
            }
            .map_err(|_| invalid("delegated child error code is invalid"))?;
            content = delegation_error_content(Some(child.child_task_id.as_str()), terminal, code);
            status = ToolResultStatus::Error;
        }
    } else {
        let has_child = state.child_tasks.values().any(|child| {
            child.session_id == pending.session_id
                && child.parent_run_id == pending.run_id
                && child.parent_tool_call_id.as_str() == call.id.as_str()
        }) || state.child_histories.iter().any(|((owner, _), history)| {
            owner == &pending.session_id
                && history.child_tasks.iter().any(|child| {
                    child.parent_run_id == pending.run_id
                        && child.parent_tool_call_id.as_str() == call.id.as_str()
                })
        });
        if has_child {
            return Err(invalid("delegated child directory is missing"));
        }
        content = delegation_error_content(
            None,
            ChildTaskStatus::Interrupted,
            serde_json::json!("interrupted"),
        );
        status = ToolResultStatus::Error;
    }
    Ok(ToolMessage {
        id: recovered_message_id(receipt, index)?,
        result: ToolResult {
            call_id: call.id.clone(),
            status,
            content,
            metadata: None,
        },
    })
}

fn delegation_error_content(
    task_id: Option<&str>,
    status: ChildTaskStatus,
    code: serde_json::Value,
) -> ToolResultContent {
    ToolResultContent::json(
        serde_json::json!({"error":{"message":"child task did not complete","details":{"task_id":task_id,"status":status,"code":code}}}),
    )
}

fn recovered_message_id(receipt: &str, index: usize) -> Result<MessageId, StoreError> {
    let sequence = index
        .checked_add(1)
        .ok_or_else(|| invalid("recovered tool message sequence overflows"))?;
    MessageId::new(format!("toolmsg_{receipt}_{sequence}"))
        .map_err(|_| invalid("recovered tool message identity is invalid"))
}

fn invalid(message: &'static str) -> StoreError {
    StoreError::new(StoreErrorKind::InvalidData, message)
}
