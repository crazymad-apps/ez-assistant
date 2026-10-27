//! 可重建的会话用量快照；只保存统计数值，不保存消息或 Step。

use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ProviderContextEffect, TokenUsage,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredConversationUsage {
    pub accumulated: Option<TokenUsage>,
    pub completed_tokens: Option<u64>,
    pub pending_tokens: u64,
    pub calibration_pending: bool,
}

impl StoredConversationUsage {
    pub fn used_tokens(&self) -> Option<u64> {
        self.completed_tokens
            .map(|tokens| tokens.saturating_add(self.pending_tokens))
    }

    /// 仅在恢复、裁剪或换代时从有效窗口校准当前占用；累计统计不随窗口裁剪重算。
    pub fn replace_context(&mut self, snapshot: &ConversationSnapshot) {
        let usage = agent_context::context_token_usage(snapshot);
        self.completed_tokens = usage.completed_tokens;
        self.pending_tokens = usage.pending_tokens;
        self.calibration_pending = false;
        for message in &snapshot.messages {
            match message {
                ConversationMessage::ContextSummary(_) => self.calibration_pending = true,
                ConversationMessage::Assistant(message)
                    if message.purpose.is_model_response() && message.usage.is_some() =>
                {
                    self.calibration_pending = false;
                }
                ConversationMessage::Assistant(message) if !message.purpose.is_model_response() => {
                    self.calibration_pending = true;
                }
                _ => {}
            }
        }
    }

    /// 可靠追加后的统计推进；调用方与消息提交处于同一事务，不能重放未提交尾帧。
    pub fn append_context(&mut self, message: &ConversationMessage) {
        let boundary = matches!(message, ConversationMessage::ContextSummary(_))
            || matches!(message, ConversationMessage::Assistant(assistant) if assistant.parts.iter().any(|part|
                matches!(part, AssistantPart::ProviderState(state) if state.context_effect() == Some(ProviderContextEffect::ReplacePriorContext))));
        if boundary {
            self.completed_tokens = None;
            self.pending_tokens = 0;
            self.calibration_pending = true;
        }
        if let ConversationMessage::Assistant(assistant) = message
            && assistant.purpose.is_model_response()
        {
            self.completed_tokens = assistant.usage.as_ref().map(|usage| usage.total_tokens);
            self.pending_tokens = 0;
            if assistant.usage.is_some() {
                self.calibration_pending = false;
            }
        } else {
            self.pending_tokens = self.pending_tokens.saturating_add(
                agent_context::context_token_usage(&ConversationSnapshot::new(vec![
                    message.clone(),
                ]))
                .pending_tokens,
            );
        }
    }

    /// 只接纳账本确认首次写入的调用；重试及恢复不能重复累计。
    pub fn accumulate(&mut self, usage: &TokenUsage) {
        let Some(total) = &mut self.accumulated else {
            self.accumulated = Some(usage.clone());
            return;
        };
        total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
        total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
        total.total_tokens = total.total_tokens.saturating_add(usage.total_tokens);
        if let Some(cached) = usage.cached_input_tokens {
            total.cached_input_tokens = Some(
                total
                    .cached_input_tokens
                    .unwrap_or(0)
                    .saturating_add(cached),
            );
        }
        if let Some(reasoning) = usage.reasoning_tokens {
            total.reasoning_tokens = Some(
                total
                    .reasoning_tokens
                    .unwrap_or(0)
                    .saturating_add(reasoning),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_types::*;

    #[test]
    fn compaction_stays_uncalibrated_until_a_response_reports_usage() {
        let boundary = ConversationMessage::ContextSummary(ContextSummaryMessage {
            id: MessageId::new("summary").unwrap(),
            text: "summary".into(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: None,
            retained_user_message_id: None,
            programmatic_context: None,
        });
        let mut response = AssistantMessage {
            id: MessageId::new("response").unwrap(),
            purpose: AssistantMessagePurpose::ModelResponse,
            model: ModelIdentity::new(ProviderId::new("test").unwrap(), "test"),
            parts: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        };
        let mut usage = StoredConversationUsage::default();
        usage.append_context(&boundary);
        usage.append_context(&ConversationMessage::Assistant(response.clone()));
        assert!(usage.calibration_pending);
        assert_eq!(usage.used_tokens(), None);
        let mut restored = StoredConversationUsage::default();
        restored.replace_context(&ConversationSnapshot::new(vec![
            boundary,
            ConversationMessage::Assistant(response.clone()),
        ]));
        assert_eq!(restored, usage);
        response.usage = Some(TokenUsage {
            input_tokens: 100,
            output_tokens: 10,
            total_tokens: 110,
            cached_input_tokens: None,
            reasoning_tokens: None,
        });
        usage.append_context(&ConversationMessage::Assistant(response));
        assert!(!usage.calibration_pending);
        assert_eq!(usage.used_tokens(), Some(110));
    }
}
