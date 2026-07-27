//! 一次 Agent 执行的输入。

use agent_types::{ConversationSnapshot, UserMessage};

/// 一次 Agent 执行的输入：既有对话快照加本轮用户输入。
///
/// 快照由 Runtime 从持久化会话生成；Core 不自行加载或持久化 Session。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionInput {
    /// 执行开始时的规范对话快照（不含 `user_input`）。
    pub conversation: ConversationSnapshot,
    /// 本轮新的用户输入。
    pub user_input: UserMessage,
}

#[cfg(test)]
mod tests {
    use agent_types::{ConversationMessage, MessageId, PartId, TextPart, UserPart};

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
            user_input: UserMessage {
                id: MessageId::new("message_2").expect("valid message id"),
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("text_2").expect("valid part id"),
                    text: "Tell me a joke.".to_owned(),
                })],
            },
        };
        let json = serde_json::to_string(&input).expect("serialize input");
        assert_eq!(
            serde_json::from_str::<ExecutionInput>(&json).expect("deserialize input"),
            input
        );
    }
}
