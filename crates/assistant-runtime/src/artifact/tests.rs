use super::*;
use agent_types::{
    AssistantMessage, ContextSummaryMessage, FinishReason, MessageId, ModelIdentity, ProviderId,
    ToolCall, ToolCallId, ToolMessage, ToolResult, ToolResultContent,
};
use assistant_protocol::SessionResourceRoot;
use serde_json::json;

fn exchange(
    id: &str,
    action: &str,
    title: &str,
    status: ToolResultStatus,
    name: &str,
) -> Vec<ConversationMessage> {
    let call_id = ToolCallId::new(id).unwrap();
    vec![
        ConversationMessage::Assistant(AssistantMessage {
            purpose: agent_types::AssistantMessagePurpose::ModelResponse,
            id: MessageId::new(format!("assistant-{id}")).unwrap(),
            model: ModelIdentity::new(ProviderId::new("fixture").unwrap(), "fixture"),
            parts: vec![AssistantPart::ToolCall(ToolCall {
                id: call_id.clone(),
                name: ToolName::new(name).unwrap(),
                arguments: json!({}),
            })],
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        }),
        ConversationMessage::Tool(ToolMessage {
            id: MessageId::new(format!("tool-{id}")).unwrap(),
            result: ToolResult {
                call_id,
                status,
                metadata: None,
                content: ToolResultContent::json(json!({
                    "action":action,"title":title,"locator":{"root":{"type":"session_private"},"relative_path":"final.txt"},"source_run_id":"run-fixture"
                })),
            },
        }),
    ]
}

#[test]
fn only_paired_successes_change_delivery_identity_and_compaction_preserves_it() {
    let mut messages = exchange(
        "first",
        "declare",
        "final",
        ToolResultStatus::Success,
        TOOL_NAME,
    );
    messages.extend(exchange(
        "script",
        "declare",
        "helper",
        ToolResultStatus::Success,
        "shell_exec",
    ));
    messages.extend(exchange(
        "failed",
        "withdraw",
        "",
        ToolResultStatus::Error,
        TOOL_NAME,
    ));
    let original = ConversationSnapshot::new(messages);
    let artifacts = project_artifacts(&original);
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].title, "final");
    let replacement = ConversationSnapshot::new(vec![ConversationMessage::ContextSummary(
        ContextSummaryMessage {
            retained_user_message_id: None,
            id: MessageId::new("summary").unwrap(),
            text: "compacted".into(),
            model: None,
            usage: None,
            compacted_usage: None,
            usage_adjustment: None,
            programmatic_context: None,
        },
    )]);
    let history =
        crate::merge_context_replacement_with_product_history(&original, &replacement).unwrap();
    assert_eq!(project_artifacts(&history), artifacts);
    assert!(project_artifacts(&crate::execution_context_from_product_history(&history)).is_empty());
    let mut history = history;
    history.messages.extend(exchange(
        "update",
        "declare",
        "updated",
        ToolResultStatus::Success,
        TOOL_NAME,
    ));
    assert_eq!(project_artifacts(&history).len(), 1);
    assert_eq!(project_artifacts(&history)[0].title, "updated");
    history.messages.extend(exchange(
        "withdraw",
        "withdraw",
        "",
        ToolResultStatus::Success,
        TOOL_NAME,
    ));
    assert!(project_artifacts(&history).is_empty());
    assert!(project_artifacts(&ConversationSnapshot::new(Vec::new())).is_empty());
}

struct Files;
impl ArtifactFiles for Files {
    fn locate(&self, path: &str) -> Result<SessionResourceLocator, ToolError> {
        Ok(SessionResourceLocator {
            root: SessionResourceRoot::SessionPrivate,
            relative_path: path.to_owned(),
        })
    }
    fn check<'a>(&'a self, _: &'a SessionResourceLocator) -> ToolExecuteFuture<'a, ()> {
        Box::pin(async { Err(ToolError::execution("missing file")) })
    }
}
#[tokio::test]
async fn declarations_require_readable_file_but_withdrawals_do_not_and_cancel_cannot_succeed() {
    let tool = DeclareArtifactTool::new(Arc::new(Files), RunId::new("run").unwrap());
    assert!(
        tool.resolve(ArtifactInput {
            action: ArtifactAction::Declare,
            path: "final".into(),
            title: None
        })
        .is_err()
    );
    let input = ArtifactDeclaration {
        action: ArtifactAction::Declare,
        title: "final".into(),
        locator: Files.locate("final").unwrap(),
        source_run_id: RunId::new("run").unwrap(),
    };
    assert!(
        tool.execute(input.clone(), ToolContext::default())
            .await
            .is_err()
    );
    let withdraw = ArtifactDeclaration {
        action: ArtifactAction::Withdraw,
        ..input
    };
    assert!(
        tool.execute(withdraw.clone(), ToolContext::default())
            .await
            .is_ok()
    );
    let cancelled = ToolContext::default();
    cancelled.cancellation.cancel();
    assert!(tool.execute(withdraw, cancelled).await.is_err());
}
