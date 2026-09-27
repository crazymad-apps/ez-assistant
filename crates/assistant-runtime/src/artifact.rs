//! 正式交付声明以成功 Tool Result 为唯一事实，列表从完整产品历史派生。

use std::{collections::HashMap, sync::Arc};

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::{
    AssistantPart, ConversationMessage, ConversationSnapshot, ToolName, ToolResultStatus,
};
use assistant_protocol::{RunId, SessionArtifact, SessionResourceLocator};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Host 为单个 Session 冻结的文件边界；不授予任意 Host 文件读取权限。
pub trait ArtifactFiles: Send + Sync {
    /// 纯词法定位；撤销缺失文件也必须使用同一稳定定位。
    fn locate(&self, path: &str) -> Result<SessionResourceLocator, ToolError>;
    /// 声明时核验根内可受控读取的普通文件，不读取或返回正文。
    fn check<'a>(&'a self, locator: &'a SessionResourceLocator) -> ToolExecuteFuture<'a, ()>;
}

const TOOL_NAME: &str = "declare_artifact";

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactAction {
    Declare,
    Withdraw,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactInput {
    action: ArtifactAction,
    /// 文件在 Host 上的绝对路径，或相对于当前工作目录的路径。
    path: String,
    /// declare 必填；withdraw 可省略。
    title: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ArtifactDeclaration {
    action: ArtifactAction,
    locator: SessionResourceLocator,
    title: String,
    source_run_id: RunId,
}

pub(crate) struct ArtifactAuthorizationFacts;

pub(crate) struct DeclareArtifactTool {
    files: Arc<dyn ArtifactFiles>,
    run_id: RunId,
}

impl DeclareArtifactTool {
    pub(crate) fn new(files: Arc<dyn ArtifactFiles>, run_id: RunId) -> Self {
        Self { files, run_id }
    }
}

impl Tool for DeclareArtifactTool {
    type Input = ArtifactInput;
    type ResolvedInput = ArtifactDeclaration;
    type Output = ArtifactDeclaration;

    fn name(&self) -> ToolName {
        ToolName::new(TOOL_NAME).expect("static tool name")
    }

    fn description(&self) -> String {
        "Declare a file explicitly requested as a user deliverable in the current session's artifact list. Call after creating and verifying the final file; do not declare helper scripts, caches, temporary files, or every file you read. Supply action=declare, path and a concise title. The path must be within this session's workspace roots or private directory. Repeating a path updates its title without duplicating it. Use action=withdraw with the same path to remove its delivery status, even if the file is missing; this never deletes the file. Only the parent agent confirms final deliverables. The declaration is committed with this tool result.".to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let title = input.title.unwrap_or_default().trim().to_owned();
        if matches!(input.action, ArtifactAction::Declare)
            && (title.is_empty()
                || title.chars().count() > 200
                || title.chars().any(char::is_control))
        {
            return Err(ToolError::invalid_input(
                "declare requires a nonempty title of at most 200 characters without control characters",
            ));
        }
        let declaration = ArtifactDeclaration {
            action: input.action,
            locator: self.files.locate(&input.path)?,
            title,
            source_run_id: self.run_id.clone(),
        };
        let semantic = serde_json::to_value(&declaration)
            .map_err(|_| ToolError::invalid_input("artifact declaration is invalid"))?;
        Ok(ToolResolution::with_facts(
            declaration,
            ArtifactAuthorizationFacts,
            semantic,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::execution("artifact declaration cancelled"));
            }
            if matches!(input.action, ArtifactAction::Declare) {
                self.files.check(&input.locator).await?;
            }
            if context.cancellation.is_cancelled() {
                return Err(ToolError::execution("artifact declaration cancelled"));
            }
            // 不在这里维护第二份状态；Recorder 完成整批 exchange 后才成为可恢复事实。
            Ok(input)
        })
    }
}

pub(crate) fn project_artifacts(history: &ConversationSnapshot) -> Vec<SessionArtifact> {
    let calls = history
        .messages
        .iter()
        .filter_map(|message| {
            if let ConversationMessage::Assistant(message) = message {
                Some(message.parts.iter())
            } else {
                None
            }
        })
        .flatten()
        .filter_map(|part| match part {
            AssistantPart::ToolCall(call) if call.name.as_str() == TOOL_NAME => {
                Some(call.id.as_str())
            }
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let mut indexes = HashMap::new();
    let mut artifacts: Vec<Option<SessionArtifact>> = Vec::new();
    for message in &history.messages {
        let ConversationMessage::Tool(message) = message else {
            continue;
        };
        if message.result.status != ToolResultStatus::Success
            || !calls.contains(message.result.call_id.as_str())
        {
            continue;
        }
        let Some(value) = message.result.content.as_single_json() else {
            continue;
        };
        let Ok(declaration) = serde_json::from_value::<ArtifactDeclaration>(value.clone()) else {
            continue;
        };
        // locator 的协议编码固定；避免用分隔符拼接产生身份碰撞。
        let Ok(key) = serde_json::to_string(&declaration.locator) else {
            continue;
        };
        match declaration.action {
            ArtifactAction::Declare => {
                let artifact = SessionArtifact {
                    title: declaration.title,
                    locator: declaration.locator,
                    source_run_id: declaration.source_run_id,
                };
                if let Some(&index) = indexes.get(&key) {
                    artifacts[index] = Some(artifact);
                } else {
                    indexes.insert(key, artifacts.len());
                    artifacts.push(Some(artifact));
                }
            }
            ArtifactAction::Withdraw => {
                if let Some(index) = indexes.remove(&key) {
                    artifacts[index] = None;
                }
            }
        }
    }
    artifacts.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests;
