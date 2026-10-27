//! 只在当前冻结目录允许时读取内置操作手册，不参与 Skill 激活或维护历史副本。

use std::sync::Arc;

use agent_tools::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution};
use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    SkillCatalog, SkillDefinition, SkillName, SkillPackageSource, SkillReferenceRequest,
    SkillSource,
};

/// 内置手册的逻辑名，由 Host 交付并沿现有目录归并规则选择。
pub const BUILTIN_GUIDE_SKILL_NAME: &str = "ez-assistant-guide";
/// 参考页键的封闭契约；Host 负责将这些键映射到同源正文。
pub const BUILTIN_GUIDE_REFERENCES: [&str; 4] = [
    "references/environment.md",
    "references/skills.md",
    "references/mcp.md",
    "references/plugin-development.md",
];
/// 一次参考页正文的 UTF-8 字节上限，不截断指令。
pub const MAX_SKILL_REFERENCE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadSkillReferenceInput {
    name: String,
    reference: String,
}

#[derive(Serialize)]
pub(crate) struct ReadSkillReferenceOutput {
    name: String,
    reference: String,
    content: String,
}

/// 仅在本工具以冻结 Winner 和静态键成功解析后产生，不放行任何后续副作用。
pub(crate) struct ReadSkillReferenceAuthorizationFacts;

pub(crate) struct ReadSkillReferenceTool {
    definition: SkillDefinition,
    source: Arc<dyn SkillPackageSource>,
}

impl ReadSkillReferenceTool {
    pub(crate) fn for_catalog(
        catalog: &SkillCatalog,
        source: Arc<dyn SkillPackageSource>,
    ) -> Option<Self> {
        let name =
            SkillName::parse(BUILTIN_GUIDE_SKILL_NAME.to_owned()).expect("static skill name");
        let definition = catalog.model_definition(&name).ok()?;
        (definition.source == SkillSource::Builtin).then(|| Self {
            definition: definition.clone(),
            source,
        })
    }
}

impl Tool for ReadSkillReferenceTool {
    type Input = ReadSkillReferenceInput;
    type ResolvedInput = ReadSkillReferenceInput;
    type Output = ReadSkillReferenceOutput;

    fn name(&self) -> ToolName {
        ToolName::new("read_skill_reference").expect("static tool name")
    }

    fn description(&self) -> String {
        format!(
            "Read one offline EZ Assistant guide topic without activating another skill. name must be {BUILTIN_GUIDE_SKILL_NAME}. reference must be one of: {}. Keys are virtual references, not filesystem paths. Reading guidance does not grant permission for any subsequent operation.",
            BUILTIN_GUIDE_REFERENCES.join(", ")
        )
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        if input.name != self.definition.name.as_str()
            || !BUILTIN_GUIDE_REFERENCES.contains(&input.reference.as_str())
        {
            return Err(ToolError::invalid_input(
                "skill name or reference is not available in the current guide; use the exact keys listed in the tool definition",
            ));
        }
        let arguments = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("skill reference input could not be resolved"))?;
        Ok(ToolResolution::with_facts(
            input,
            ReadSkillReferenceAuthorizationFacts,
            arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::execution("skill reference read cancelled"));
            }
            let request = SkillReferenceRequest {
                name: self.definition.name.clone(),
                definition_digest: self.definition.definition_digest.clone(),
                reference: input.reference.clone(),
            };
            let result = tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => return Err(ToolError::execution("skill reference read cancelled")),
                result = self.source.read_reference(request) => result,
            };
            let content = result
                .map_err(|_| ToolError::execution("skill reference source is unavailable"))?
                .ok_or_else(|| {
                    ToolError::execution(
                        "skill reference is unavailable for this frozen definition",
                    )
                })?;
            if context.cancellation.is_cancelled() {
                return Err(ToolError::execution("skill reference read cancelled"));
            }
            if content.len() > MAX_SKILL_REFERENCE_BYTES {
                return Err(ToolError::execution(
                    "skill reference exceeds the complete-content limit",
                ));
            }
            Ok(ReadSkillReferenceOutput {
                name: input.name,
                reference: input.reference,
                content,
            })
        })
    }
}
