//! 一次 Agent 执行的输入。

use agent_types::ConversationSnapshot;

/// 一次 Agent 执行的完整规范对话输入。
///
/// 快照由 Runtime 从有效会话历史生成，已经包含本次执行所需的用户输入；
/// Core 不追加消息，也不自行加载或持久化 Session。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionInput {
    /// 执行开始时的完整规范对话快照。
    pub conversation: ConversationSnapshot,
}

#[cfg(test)]
mod tests {
    use agent_types::{ConversationMessage, MessageId, PartId, TextPart, UserMessage, UserPart};

    use super::*;

    #[test]
    fn input_round_trips_serde() {
        let input = ExecutionInput {
            conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
                id: MessageId::new("message_1").expect("valid message id"),
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("text_1").expect("valid part id"),
                    text: "What date is it?".to_owned(),
                })],
            })]),
        };
        let json = serde_json::to_string(&input).expect("serialize input");
        assert_eq!(
            serde_json::from_str::<ExecutionInput>(&json).expect("deserialize input"),
            input
        );
    }
}
