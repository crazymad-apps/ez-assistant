//! 快照与流式历史共用的工具交换结构校验；只保留调用 ID，不持有消息正文。

use std::collections::HashSet;

use super::{
    AssistantPart, ConversationMessage, ConversationValidationError, ProviderContextEffect,
};
use crate::ToolCallId;

/// 同一 Conversation 的增量校验器。调用者必须保持消息顺序，首次错误后停止读取，
/// 并在 EOF 调用 `finish`；只有完整交换边界才可切分一个可靠消息批次。
/// 校验规则与 `ConversationSnapshot::validate_tool_exchange_pairs` 完全共用，不执行 I/O。
#[derive(Clone, Debug, Default)]
pub struct ToolExchangeValidator {
    seen_calls: HashSet<ToolCallId>,
    completed_calls: HashSet<ToolCallId>,
    pending_calls: Vec<ToolCallId>,
    next_result: usize,
}

impl ToolExchangeValidator {
    /// 接纳下一条规范消息；摘要与原生边界只有在上一交换完整时才重置调用 ID 作用域。
    ///
    /// # Errors
    /// 重复、缺失或顺序不符的调用／结果，以及截断交换的边界返回结构错误。
    pub fn push(
        &mut self,
        message: &ConversationMessage,
    ) -> Result<(), ConversationValidationError> {
        match message {
            ConversationMessage::Tool(message) => {
                let actual = &message.result.call_id;
                if self.pending_calls.is_empty() {
                    return if self.completed_calls.contains(actual) {
                        Err(ConversationValidationError::DuplicateToolResult {
                            call_id: actual.clone(),
                        })
                    } else {
                        Err(ConversationValidationError::ToolResultWithoutCall {
                            call_id: actual.clone(),
                        })
                    };
                }

                let expected = &self.pending_calls[self.next_result];
                if actual != expected {
                    return if self.completed_calls.contains(actual) {
                        Err(ConversationValidationError::DuplicateToolResult {
                            call_id: actual.clone(),
                        })
                    } else if self.pending_calls[self.next_result + 1..].contains(actual) {
                        Err(ConversationValidationError::ToolResultOutOfOrder {
                            expected: expected.clone(),
                            actual: actual.clone(),
                        })
                    } else {
                        Err(ConversationValidationError::ToolResultWithoutCall {
                            call_id: actual.clone(),
                        })
                    };
                }

                self.completed_calls.insert(actual.clone());
                self.next_result += 1;
                if self.next_result == self.pending_calls.len() {
                    self.pending_calls.clear();
                    self.next_result = 0;
                }
            }
            non_tool => {
                if let Some(missing) = self.pending_calls.get(self.next_result) {
                    return Err(ConversationValidationError::MissingToolResult {
                        call_id: missing.clone(),
                    });
                }

                // 摘要之前的消息不再进入后续模型请求，不能占用新上下文的调用 ID。
                // 必须先确认上一批结果完整，不能用摘要掩盖未配对的调用。
                if matches!(message, ConversationMessage::ContextSummary(_)) {
                    self.seen_calls.clear();
                    self.completed_calls.clear();
                }

                if let ConversationMessage::Assistant(message) = non_tool {
                    for part in &message.parts {
                        if let AssistantPart::ProviderState(state) = part
                            && state.context_effect()
                                == Some(ProviderContextEffect::ReplacePriorContext)
                        {
                            // 原生边界只能重置完整交换；不能抹掉同一响应前面的待执行调用。
                            if !self.pending_calls.is_empty() {
                                return Err(
                                    ConversationValidationError::InvalidProviderContextBoundary,
                                );
                            }
                            self.seen_calls.clear();
                            self.completed_calls.clear();
                        }
                        if let AssistantPart::ToolCall(call) = part {
                            if !self.seen_calls.insert(call.id.clone()) {
                                return Err(ConversationValidationError::DuplicateToolCallId {
                                    call_id: call.id.clone(),
                                });
                            }
                            self.pending_calls.push(call.id.clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 当前是否处于完整工具交换边界；这不替代读取结束时的 `finish`。
    pub fn is_complete(&self) -> bool {
        self.pending_calls.is_empty()
    }

    /// 结束完整输入，拒绝遗留未配对调用。没有外部副作用或执行恢复能力。
    ///
    /// # Errors
    /// EOF 仍缺少 Tool Result 时返回对应调用的结构错误。
    pub fn finish(&self) -> Result<(), ConversationValidationError> {
        if let Some(missing) = self.pending_calls.get(self.next_result) {
            return Err(ConversationValidationError::MissingToolResult {
                call_id: missing.clone(),
            });
        }
        Ok(())
    }
}
