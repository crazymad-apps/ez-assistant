//! 正式用户域的反馈工具及默认权限规则；不新增 Core 能力或审批路径。
use crate::access::enterprise::feedback::{FeedbackAccess, normalize_content};
use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use assistant_protocol::{
    FeedbackClientEnvironment, FeedbackClientKind, FeedbackContent, FeedbackFailure,
    FeedbackSource, SubmitFeedbackRequest,
};
use assistant_runtime::{
    GeneralPermissionMatcher, PermissionDocument, PermissionEffect, PermissionMatcher,
    PermissionRule, PermissionRuleSource, PermissionStoreFuture,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    target: Target,
    /// 简短标题，最多 120 字。
    title: String,
    /// 问题描述，最多 4000 字；只提供理解问题所需内容。
    description: String,
    reproduction: Option<String>,
    expected_result: Option<String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Target {
    kind: Kind,
    /// 平台为 ez-assistant；插件填稳定标识；知识库可填来源引用，未知时留空。
    reference: Option<String>,
    name: String,
    /// 仅插件允许提供已知的实际使用版本。
    version: Option<String>,
    /// 具体文档或条目的来源引用。
    content_ref: Option<String>,
    /// 功能、章节或可复核的位置。
    location: Option<String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Platform,
    Plugin,
    Knowledge,
}

pub(super) struct SubmitFeedbackTool(pub(super) FeedbackAccess);
impl Tool for SubmitFeedbackTool {
    type Input = Input;
    type ResolvedInput = FeedbackContent;
    type Output = serde_json::Value;
    fn name(&self) -> ToolName {
        ToolName::new("submit_feedback").expect("valid fixed tool name")
    }
    fn description(&self) -> String {
        "向反馈中心提交平台、插件或知识库问题。可接受用户委托，也可自主记录已确认的问题；提供结构化目标和最少必要正文，不包含密码、令牌、完整会话或无关文件。成功以返回的反馈编号为准。结果未知时不得自动重试；每次新工具调用都可能创建另一条反馈，可让用户在反馈中心核对。提交失败后继续原任务，不要为反馈失败再调用本工具递归上报。".into()
    }
    fn resolve(&self, input: Input) -> Result<ToolResolution<FeedbackContent>, ToolError> {
        let target = serde_json::from_value(
            serde_json::to_value(input.target)
                .map_err(|_| ToolError::invalid_input("反馈目标无效"))?,
        )
        .map_err(|_| ToolError::invalid_input("反馈目标无效"))?;
        let content = normalize_content(FeedbackContent {
            target,
            title: input.title,
            description: input.description,
            reproduction: input.reproduction,
            expected_result: input.expected_result,
        })
        .map_err(|error| {
            ToolError::invalid_input(match error {
                FeedbackFailure::TooLarge => "反馈内容超过 12 KiB，请精简",
                _ => "反馈目标或正文无效，请检查字段及长度",
            })
        })?;
        Ok(ToolResolution::general(content))
    }
    fn execute<'a>(
        &'a self,
        content: FeedbackContent,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::execution("反馈提交已取消"));
            }
            let submission_id = submission_id()?;
            let request = SubmitFeedbackRequest {
                submission_id: submission_id.clone(),
                content,
                client: FeedbackClientEnvironment {
                    client_kind: FeedbackClientKind::Agent,
                    client_version: None,
                    client_os: None,
                    client_arch: None,
                },
            };
            let result = tokio::select! { biased;
                _ = context.cancellation.cancelled() => Err(FeedbackFailure::SubmissionUnknown),
                result = self.0.submit(request, FeedbackSource::Agent) => result,
            };
            match result {
                Ok(detail) => Ok(
                    serde_json::json!({"id":detail.summary.id,"submission_id":detail.submission_id,"status":detail.summary.status,"target":detail.target}),
                ),
                Err(error) => Err(ToolError::execution_with_details(
                    match error {
                        FeedbackFailure::SubmissionUnknown => {
                            "尚未确认反馈提交结果，请在反馈中心核对；不要自动重试。"
                        }
                        FeedbackFailure::Unsupported => {
                            "当前中心未开放反馈。请继续原任务，不要递归上报。"
                        }
                        FeedbackFailure::IdentityChanged => "当前反馈身份已失效，请重新登录。",
                        _ => "反馈未能提交，请继续原任务，不要为此次失败再提交反馈。",
                    },
                    serde_json::json!({"code":error,"submission_id":submission_id}),
                )),
            }
        })
    }
}

pub(super) struct HostDefaultRules {
    pub(super) paths: Arc<crate::user_paths::UserPaths>,
    pub(super) feedback: bool,
}
impl PermissionRuleSource for HostDefaultRules {
    fn load(&self) -> PermissionStoreFuture<'_, PermissionDocument> {
        Box::pin(async move {
            let mut document = self.paths.load().await?;
            if self.feedback {
                document.rules.push(PermissionRule {
                    id: "host-submit-feedback".into(),
                    effect: PermissionEffect::Allow,
                    variants: vec![
                        assistant_protocol::AgentVariant::Plan,
                        assistant_protocol::AgentVariant::Build,
                    ],
                    matcher: PermissionMatcher::General(GeneralPermissionMatcher {
                        tool_name: "submit_feedback".into(),
                    }),
                });
            }
            Ok(document)
        })
    }
}

fn submission_id() -> Result<String, ToolError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ToolError::execution("无法生成反馈提交标识"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_tools::{Dispatcher, GeneralAuthorizationFacts, ToolRegistry};
    use agent_types::{ToolCall, ToolCallId};
    #[tokio::test]
    async fn feedback_schema_and_resolve_reject_identity_fields_and_keep_general_permissions() {
        let tool = SubmitFeedbackTool(FeedbackAccess::new(
            Arc::new(crate::access::Credentials::new()),
            None,
        ));
        let mut registry = ToolRegistry::new();
        registry.register(tool).unwrap();
        let snapshot = registry.snapshot();
        let schema = serde_json::to_value(&snapshot.definitions()[0]).unwrap();
        assert!(schema.to_string().contains("submit_feedback"));
        let input = serde_json::json!({"target":{"kind":"platform","name":"EZ Assistant"},"title":"title","description":"description"});
        let call = ToolCall {
            id: ToolCallId::new("feedback-call").unwrap(),
            name: ToolName::new("submit_feedback").unwrap(),
            arguments: input.clone(),
        };
        let batch = Dispatcher::resolve_batch(&snapshot, &[call]);
        let agent_tools::ResolvedBatchItemRef::Valid(invocation) = batch.iter().next().unwrap()
        else {
            panic!("valid feedback input")
        };
        assert!(invocation.facts::<GeneralAuthorizationFacts>().is_some());
        let mut invalid = input.clone();
        invalid["owner_user_id"] = serde_json::json!(2);
        assert!(serde_json::from_value::<Input>(invalid).is_err());
        let tool = SubmitFeedbackTool(FeedbackAccess::new(
            Arc::new(crate::access::Credentials::new()),
            None,
        ));
        let input: Input = serde_json::from_value(input).unwrap();
        let content = tool.resolve(input).unwrap();
        // resolve 在没有中心配置时仍可完成；实际个人域不会注册该工具。
        let _ = content;
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let value = FeedbackContent {
            target: assistant_protocol::FeedbackTarget {
                kind: assistant_protocol::FeedbackTargetKind::Platform,
                reference: None,
                name: "EZ Assistant".into(),
                version: None,
                content_ref: None,
                location: None,
            },
            title: "title".into(),
            description: "description".into(),
            reproduction: None,
            expected_result: None,
        };
        assert!(
            tool.execute(value, ToolContext::new(cancellation, Arc::new(|_| {})))
                .await
                .is_err()
        );
    }
    #[test]
    fn independent_tool_executions_use_distinct_canonical_submission_ids() {
        let a = submission_id().unwrap();
        let b = submission_id().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
    }
}
