//! 冷扫描与 Journal 共用的执行窗口归并；只保留最近有效压缩点之后的正文。

use std::collections::BTreeMap;

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ProviderContextEffect,
};

use crate::{StoreError, StoreErrorKind, context_compaction::memory::PinnedMemoryContext};

/// 从可靠历史顺序恢复、随后由 Journal 独占维护的当前执行上下文。
/// 不持久化第二份正文；旧正文在有效压缩点立即释放，必要的净记忆与能力状态单独归并。
#[derive(Clone, Debug, Default)]
pub struct ExecutionWindow {
    pub(crate) conversation: ConversationSnapshot,
    memory: PinnedMemoryContext,
    capabilities: BTreeMap<String, String>,
    pub(crate) artifacts: crate::artifact::ArtifactProjection,
    pub(crate) file_references: Vec<assistant_protocol::ConversationFileReference>,
    pending_files: Option<agent_types::AssistantMessage>,
}

impl ExecutionWindow {
    /// 消费一条已校验顺序的可靠消息；完整文件校验由 Store 负责，业务只能在扫描结束后开始。
    ///
    /// # Errors
    /// 压缩边界或记忆工具事实非法时拒绝恢复。
    pub fn push(&mut self, message: ConversationMessage) -> Result<(), StoreError> {
        self.push_message(message, true)
    }

    pub(crate) fn append_uncommitted(
        &mut self,
        message: ConversationMessage,
    ) -> Result<(), StoreError> {
        self.push_message(message, false)
    }

    fn push_message(&mut self, message: ConversationMessage, trim: bool) -> Result<(), StoreError> {
        self.memory.push(&message).map_err(context_error)?;
        self.artifacts.push(&message);
        match &message {
            ConversationMessage::Assistant(assistant) => {
                self.pending_files = Some(assistant.clone())
            }
            ConversationMessage::Tool(_) => {
                if let Some(assistant) = &self.pending_files {
                    self.file_references.extend(
                        crate::runtime::product::project_conversation_file_references(
                            &ConversationSnapshot::new(vec![
                                ConversationMessage::Assistant(assistant.clone()),
                                message.clone(),
                            ]),
                        )
                        .map_err(context_error)?,
                    );
                }
            }
            _ => self.pending_files = None,
        }

        if let ConversationMessage::User(user) = &message {
            for part in &user.parts {
                if let agent_types::UserPart::InternalContext(part) = part
                    && matches!(
                        part.kind.as_str(),
                        "mcp_server_directory" | "mcp_server_selection" | "shell_environment"
                    )
                {
                    self.capabilities
                        .insert(part.kind.clone(), part.text.clone());
                }
            }
        }
        if trim {
            let prefix = self
                .conversation
                .messages
                .iter()
                .take_while(|message| matches!(message, ConversationMessage::System(_)))
                .count();
            let retained = match &message {
                ConversationMessage::ContextSummary(summary) => summary
                    .retained_user_message_id
                    .as_ref()
                    .and_then(|id| {
                        self.conversation.messages.iter().find(|message|
                        matches!(message, ConversationMessage::User(user) if &user.id == id))
                    })
                    .cloned(),
                _ => None,
            };
            match message {
                ConversationMessage::ContextSummary(summary)
                    if summary.retained_user_message_id.is_none() || retained.is_some() => {
                    self.conversation.messages.truncate(prefix);
                    self.conversation.messages.push(ConversationMessage::ContextSummary(summary));
                    self.conversation.messages.extend(retained);
                }
                ConversationMessage::Assistant(mut assistant) if assistant.parts.iter().any(|part|
                    matches!(part, AssistantPart::ProviderState(state) if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext))) => {
                    let boundary = assistant.parts.iter().rposition(|part|
                        matches!(part, AssistantPart::ProviderState(state) if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext))).expect("boundary checked");
                    assistant.parts.drain(..boundary);
                    self.conversation.messages.truncate(prefix);
                    self.conversation.messages.push(ConversationMessage::Assistant(assistant));
                }
                message => self.conversation.messages.push(message),
            }
        } else {
            self.conversation.messages.push(message);
        }
        Ok(())
    }

    /// 仅供已结束扫描的调用方取得执行快照；不读取文件，也不装配历史子任务。
    pub fn snapshot(&self) -> ConversationSnapshot {
        self.conversation.clone()
    }

    /// 对扫描结果执行最后结构校验，成功后才可安装到 Session。
    ///
    /// # Errors
    /// 未闭合工具交换或压缩引用损坏时失败。
    pub fn finish(&mut self) -> Result<(), StoreError> {
        self.conversation = self
            .conversation
            .project_provider_context()
            .map_err(context_error)?;
        self.instructions()?;
        Ok(())
    }

    pub(crate) fn from_snapshot(snapshot: ConversationSnapshot) -> Result<Self, StoreError> {
        let mut window = Self::default();
        for message in snapshot.messages {
            window.push(message)?;
        }
        window.finish()?;
        Ok(window)
    }

    pub(crate) fn instructions(&self) -> Result<Vec<String>, StoreError> {
        let mut instructions: Vec<_> = self
            .memory
            .render()
            .map_err(context_error)?
            .into_iter()
            .collect();
        instructions.extend(self.capabilities.values().cloned());
        Ok(instructions)
    }
}

fn context_error(error: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::with_source(
        StoreErrorKind::InvalidData,
        "execution window is invalid",
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_types::*;

    fn user(id: &str) -> ConversationMessage {
        ConversationMessage::User(UserMessage {
            id: MessageId::new(id).unwrap(),
            origin: Default::default(),
            transcript_visibility: Default::default(),
            parts: Vec::new(),
        })
    }

    #[test]
    fn each_checkpoint_releases_covered_messages_before_eof() {
        let mut window = ExecutionWindow::default();
        for round in 0..10 {
            for item in 0..100 {
                window.push(user(&format!("{round}-{item}"))).unwrap();
            }
            window
                .push(ConversationMessage::ContextSummary(ContextSummaryMessage {
                    id: MessageId::new(format!("summary-{round}")).unwrap(),
                    text: "current summary".into(),
                    model: None,
                    usage: None,
                    compacted_usage: None,
                    usage_adjustment: None,
                    retained_user_message_id: Some(MessageId::new(format!("{round}-99")).unwrap()),
                    programmatic_context: None,
                }))
                .unwrap();
            assert_eq!(
                window.conversation.messages.len(),
                2,
                "covered messages must be released before scanning the next record"
            );
        }
        window.push(user("latest")).unwrap();
        window.finish().unwrap();
        assert_eq!(window.conversation.messages.len(), 3);
    }

    #[test]
    fn native_boundary_can_be_followed_by_its_tool_results() {
        let mut window = ExecutionWindow::default();
        window.push(user("old")).unwrap();
        let state = OpaqueProviderState::new_routed(
            ProviderId::new("openai").unwrap(),
            ProtocolId::new("responses").unwrap(),
            "responses.compacted_window",
            "application/json",
            1,
            None,
            "a".repeat(64),
            b"{}".to_vec(),
        )
        .unwrap()
        .with_context_effect(ProviderContextEffect::ReplacePriorContext)
        .unwrap();
        let call_id = ToolCallId::new("call").unwrap();
        window
            .push(ConversationMessage::Assistant(AssistantMessage {
                id: MessageId::new("boundary").unwrap(),
                purpose: AssistantMessagePurpose::ModelResponse,
                model: ModelIdentity::new(ProviderId::new("openai").unwrap(), "test"),
                parts: vec![
                    AssistantPart::ProviderState(state),
                    AssistantPart::ToolCall(ToolCall {
                        id: call_id.clone(),
                        name: ToolName::new("read_file").unwrap(),
                        arguments: serde_json::json!({}),
                    }),
                ],
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            }))
            .unwrap();
        assert_eq!(window.conversation.messages.len(), 1);
        window
            .push(ConversationMessage::Tool(ToolMessage {
                id: MessageId::new("result").unwrap(),
                result: ToolResult {
                    call_id,
                    status: ToolResultStatus::Success,
                    content: ToolResultContent::text("result"),
                    metadata: None,
                },
            }))
            .unwrap();
        window.finish().unwrap();
        assert_eq!(window.conversation.messages.len(), 2);
    }
}
