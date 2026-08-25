//! 从规范 Tool Result 批次生成请求期插入计划。

use agent_types::{
    AssistantMessage, ContextInsertionPlan, MessageId, ToolMessage, ToolResultImageInsertion,
    ToolResultPart,
};

const TOOL_RESULT_IMAGE_PLACEHOLDER_VERSION: &str = "tool_result_image";

/// request-only 图片信封所依据的完整规范批次身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResultImageBatchIdentity {
    pub assistant_message_id: MessageId,
    pub tool_message_ids: Vec<MessageId>,
}

/// 按 Tool Call 与 Part 顺序规划完整 Tool Result 批次后的图片信封。
///
/// 返回的计划只引用已经存在的规范消息和图片 Part，不创建 MessageId、Runtime boundary
/// 或产品事件。没有图片时不产生计划。
pub fn plan_tool_result_image_envelope(
    assistant: &AssistantMessage,
    tools: &[&ToolMessage],
) -> Option<ContextInsertionPlan<ToolResultImageBatchIdentity>> {
    let mut images = Vec::new();
    for tool in tools {
        for (part_index, part) in tool.result.content.as_parts().iter().enumerate() {
            let ToolResultPart::Image { image } = part else {
                continue;
            };
            images.push(ToolResultImageInsertion {
                tool_message_id: tool.id.clone(),
                call_id: tool.result.call_id.clone(),
                part_index,
                image: image.clone(),
                label: tool_result_image_label(tool.result.call_id.as_str(), part_index),
            });
        }
    }
    if images.is_empty() {
        return None;
    }
    Some(ContextInsertionPlan::request_only_tool_images(
        ToolResultImageBatchIdentity {
            assistant_message_id: assistant.id.clone(),
            tool_message_ids: tools.iter().map(|tool| tool.id.clone()).collect(),
        },
        images,
    ))
}

/// Tool Result 文本占位与 request-only 图片信封共用的版本化标签。
pub fn tool_result_image_label(call_id: &str, part_index: usize) -> String {
    let mut label = format!("[{TOOL_RESULT_IMAGE_PLACEHOLDER_VERSION} call_id=\"");
    for character in call_id.chars() {
        match character {
            '&' => label.push_str("&amp;"),
            '<' => label.push_str("&lt;"),
            '>' => label.push_str("&gt;"),
            '\"' => label.push_str("&quot;"),
            '\'' => label.push_str("&apos;"),
            character => label.push(character),
        }
    }
    label.push_str(&format!(
        "\" part_index=\"{part_index}\" supplied_in_following_batch]"
    ));
    label
}

#[cfg(test)]
mod tests {
    use agent_types::{
        FinishReason, ModelIdentity, ProviderId, ToolCallId, ToolImageReference, ToolResult,
        ToolResultContent, ToolResultPart, ToolResultStatus,
    };

    use super::*;

    fn image(path: &str) -> ToolResultPart {
        ToolResultPart::Image {
            image: ToolImageReference::new(path, "image/png").expect("image reference"),
        }
    }

    #[test]
    fn image_plan_preserves_tool_and_part_order_without_creating_messages() {
        let assistant = AssistantMessage {
            id: MessageId::new("assistant-1").expect("message id"),
            model: ModelIdentity::new(ProviderId::new("test").expect("provider id"), "test-model"),
            parts: Vec::new(),
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        };
        let first = ToolMessage {
            id: MessageId::new("tool-1").expect("message id"),
            result: ToolResult {
                call_id: ToolCallId::new("call<&").expect("call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![
                    ToolResultPart::Text {
                        text: "first".to_owned(),
                    },
                    image(&format!("{}.png", "a".repeat(64))),
                ])
                .expect("content"),
                metadata: None,
            },
        };
        let second = ToolMessage {
            id: MessageId::new("tool-2").expect("message id"),
            result: ToolResult {
                call_id: ToolCallId::new("call-2").expect("call id"),
                status: ToolResultStatus::Success,
                content: ToolResultContent::parts(vec![image(&format!("{}.png", "b".repeat(64)))])
                    .expect("content"),
                metadata: None,
            },
        };

        let plan =
            plan_tool_result_image_envelope(&assistant, &[&first, &second]).expect("image plan");
        assert_eq!(plan.source_identity.assistant_message_id, assistant.id);
        assert_eq!(
            plan.source_identity.tool_message_ids,
            vec![first.id, second.id]
        );
        let agent_types::ContextInsertionPayload::ToolResultImages(images) = plan.payload else {
            panic!("expected image payload");
        };
        assert_eq!(images[0].part_index, 1);
        assert_eq!(images[1].part_index, 0);
        assert!(images[0].label.contains("call&lt;&amp;"));
    }
}
