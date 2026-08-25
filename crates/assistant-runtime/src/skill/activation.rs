//! Skill Activation 事实、内部正文、执行 latch 与稳定 load_skill 工具。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use agent_tools::{
    Tool, ToolContext, ToolError, ToolExecuteFuture, ToolExecutionMode, ToolResolution,
};
use agent_types::{MessageId, ToolCallId, ToolMessage, ToolName, ToolResultStatus};
use assistant_protocol::{InputId, RunId, SessionId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ModelSkillResolveError, SessionSkillCatalog, SessionSkillDefinition, SkillName};

/// Activation 进入规范 Conversation 的触发来源。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationTrigger {
    User,
    Model,
}

/// Activation ledger 中规范 Conversation 的所有者。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SkillActivationOwner {
    Session(SessionId),
    ChildTask(String),
}

/// 随 Input 或 Tool Exchange 原子持久化的结构化 Activation 事实。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredSkillActivation {
    pub activation_id: String,
    pub session_id: SessionId,
    pub owner: SkillActivationOwner,
    pub run_id: Option<RunId>,
    pub input_id: Option<InputId>,
    pub message_id: MessageId,
    pub name: SkillName,
    pub catalog_revision: String,
    pub definition_digest: String,
    pub trigger: SkillActivationTrigger,
    pub created_at_ms: i64,
}

impl StoredSkillActivation {
    /// 生成 Queue、消息和当前上下文共用的最小产品标签。
    pub fn tag(&self) -> assistant_protocol::SkillActivationTagSnapshot {
        assistant_protocol::SkillActivationTagSnapshot {
            name: self.name.as_str().to_owned(),
        }
    }
}

/// 把冻结定义渲染为统一内部边界承载的精确模型正文。
pub(crate) fn render_user_activation(
    catalog_revision: &str,
    definition: &SessionSkillDefinition,
) -> String {
    format!(
        "SKILL_ACTIVATION_V1\ntrigger: user\nname: {}\ncatalog_revision: {}\ndefinition_digest: {}\nshared_skill_root: {}\n<skill-instructions>\n{}\n</skill-instructions>",
        definition.name.as_str(),
        catalog_revision,
        definition.definition_digest,
        definition.source_path,
        definition.body,
    )
}

/// 把模型激活的冻结定义渲染为统一内部边界正文。
pub(crate) fn render_model_activation(
    catalog_revision: &str,
    definition: &SessionSkillDefinition,
) -> String {
    format!(
        "SKILL_ACTIVATION_V1\ntrigger: model\nname: {}\ncatalog_revision: {}\ndefinition_digest: {}\nshared_skill_root: {}\n<skill-instructions>\n{}\n</skill-instructions>",
        definition.name.as_str(),
        catalog_revision,
        definition.definition_digest,
        definition.source_path,
        definition.body,
    )
}

/// 单个 AgentExecution 内暂存、并在 Recorder 完整提交后才生效的 Skill Activation。
pub(crate) struct SkillActivationLatch {
    state: Mutex<SkillActivationLatchState>,
}

#[derive(Default)]
struct SkillActivationLatchState {
    active: BTreeSet<SkillName>,
    staged: BTreeMap<ToolCallId, SessionSkillDefinition>,
}

impl SkillActivationLatch {
    pub(crate) fn new(active: impl IntoIterator<Item = SkillName>) -> Self {
        Self {
            state: Mutex::new(SkillActivationLatchState {
                active: active.into_iter().collect(),
                staged: BTreeMap::new(),
            }),
        }
    }

    pub(super) fn stage(
        &self,
        call_id: ToolCallId,
        definition: SessionSkillDefinition,
    ) -> Result<bool, ToolError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::execution("skill activation latch is unavailable"))?;
        if state.active.contains(&definition.name)
            || state
                .staged
                .values()
                .any(|candidate| candidate.name == definition.name)
        {
            return Ok(false);
        }
        state.staged.insert(call_id, definition);
        Ok(true)
    }

    /// 按 Tool Result 原顺序读取本次成功调用对应的暂存定义，不提前消费 latch。
    pub(crate) fn staged_for_results(
        &self,
        results: &[ToolMessage],
    ) -> Result<Vec<(ToolCallId, SessionSkillDefinition)>, ()> {
        let state = self.state.lock().map_err(|_| ())?;
        Ok(results
            .iter()
            .filter(|message| message.result.status == ToolResultStatus::Success)
            .filter_map(|message| {
                state
                    .staged
                    .get(&message.result.call_id)
                    .cloned()
                    .map(|definition| (message.result.call_id.clone(), definition))
            })
            .collect())
    }

    /// Store 与 Journal 均成功后，把指定暂存项转为本 execution 的已激活集合。
    pub(crate) fn commit(&self, call_ids: &[ToolCallId]) -> Result<(), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        if call_ids
            .iter()
            .any(|call_id| !state.staged.contains_key(call_id))
        {
            return Err(());
        }
        for call_id in call_ids {
            let definition = state
                .staged
                .remove(call_id)
                .expect("all staged call ids were validated before mutation");
            state.active.insert(definition.name);
        }
        Ok(())
    }
}

/// `load_skill` 只暴露一个稳定普通字符串参数，不随 Catalog 动态生成枚举。
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoadSkillInput {
    pub(super) name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LoadSkillStatus {
    Staged,
    AlreadyActive,
    NotFound,
    NotModelInvocable,
    CatalogUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct LoadSkillOutput {
    pub(super) status: LoadSkillStatus,
    name: String,
}

/// Runtime 私有派生工具授权事实；只由本类型的 resolve 构造。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoadSkillAuthorizationFacts;

pub(crate) struct LoadSkillTool {
    catalog: SessionSkillCatalog,
    latch: std::sync::Arc<SkillActivationLatch>,
}

impl LoadSkillTool {
    pub(crate) fn new(
        catalog: SessionSkillCatalog,
        latch: std::sync::Arc<SkillActivationLatch>,
    ) -> Self {
        Self { catalog, latch }
    }
}

impl Tool for LoadSkillTool {
    type Input = LoadSkillInput;
    type ResolvedInput = LoadSkillInput;
    type Output = LoadSkillOutput;

    fn name(&self) -> ToolName {
        ToolName::new("load_skill").expect("static tool name is valid")
    }

    fn description(&self) -> String {
        "Load one enabled skill from the frozen session catalog by its exact name. The skill becomes available after this complete tool batch is reliably committed."
            .to_owned()
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Serial
    }

    fn resolve(
        &self,
        mut input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        input.name = input.name.trim().to_owned();
        let semantic_arguments = serde_json::to_value(&input)
            .map_err(|_| ToolError::invalid_input("load_skill input could not be resolved"))?;
        Ok(ToolResolution::with_facts(
            input,
            LoadSkillAuthorizationFacts,
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let output = |status| LoadSkillOutput {
                status,
                name: input.name.clone(),
            };
            let name = match SkillName::parse(input.name.clone()) {
                Ok(name) => name,
                Err(_) => return Ok(output(LoadSkillStatus::NotFound)),
            };
            let definition = match self.catalog.model_definition(&name) {
                Ok(definition) => definition.clone(),
                Err(ModelSkillResolveError::CatalogUnavailable) => {
                    return Ok(output(LoadSkillStatus::CatalogUnavailable));
                }
                Err(ModelSkillResolveError::NotFound) => {
                    return Ok(output(LoadSkillStatus::NotFound));
                }
                Err(ModelSkillResolveError::NotModelInvocable) => {
                    return Ok(output(LoadSkillStatus::NotModelInvocable));
                }
            };
            let call_id = context
                .call_id()
                .cloned()
                .ok_or_else(|| ToolError::execution("load_skill call identity is unavailable"))?;
            if self.latch.stage(call_id, definition)? {
                Ok(output(LoadSkillStatus::Staged))
            } else {
                Ok(output(LoadSkillStatus::AlreadyActive))
            }
        })
    }
}
