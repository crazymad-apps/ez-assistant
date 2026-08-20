use std::sync::Arc;

use agent_context::ContextWindowEvaluator;
use agent_core::{ExecutionBudget, ExecutionSpec, GuardrailConfig, ModelRequestConfig};
use agent_model::{ModelService, SystemPromptSnapshot};
use agent_tools::ToolSetSnapshot;
use agent_types::ToolChoice;

use crate::{Agent, AgentBuildError};

/// 冻结 [`Agent`] 执行配置的薄装配器。
///
/// 模型、System Prompt 和 Context Window 是必填项；其余配置具有与底层 Core
/// 一致的显式默认值。Builder 不读取环境变量、配置文件或记忆 Store。
pub struct AgentBuilder {
    model: Arc<dyn ModelService>,
    system_prompt: SystemPromptSnapshot,
    context_window: Arc<ContextWindowEvaluator>,
    tools: ToolSetSnapshot,
    model_request: ModelRequestConfig,
    budget: ExecutionBudget,
    guardrails: Option<GuardrailConfig>,
}

impl AgentBuilder {
    /// 使用三个必填的冻结能力创建 Builder。
    pub fn new(
        model: Arc<dyn ModelService>,
        system_prompt: SystemPromptSnapshot,
        context_window: Arc<ContextWindowEvaluator>,
    ) -> Self {
        Self {
            model,
            system_prompt,
            context_window,
            tools: ToolSetSnapshot::default(),
            model_request: ModelRequestConfig::default(),
            budget: ExecutionBudget::default(),
            guardrails: None,
        }
    }

    /// 使用已经冻结的工具集合替换默认空集合。
    #[must_use]
    pub fn tools(mut self, tools: ToolSetSnapshot) -> Self {
        self.tools = tools;
        self
    }

    /// 设置每个 Model Step 复用的请求配置。
    #[must_use]
    pub fn model_request(mut self, config: ModelRequestConfig) -> Self {
        self.model_request = config;
        self
    }

    /// 设置一次执行使用的显式资源预算。
    #[must_use]
    pub fn budget(mut self, budget: ExecutionBudget) -> Self {
        self.budget = budget;
        self
    }

    /// 启用给定 Guardrail；不调用本方法表示关闭整个 Guardrail 机制。
    #[must_use]
    pub fn guardrails(mut self, guardrails: GuardrailConfig) -> Self {
        self.guardrails = Some(guardrails);
        self
    }

    /// 校验跨字段约束并冻结为 Agent 执行 Facade。
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        self.validate()?;
        Ok(Agent::from_spec(ExecutionSpec {
            system_prompt: self.system_prompt,
            model: self.model,
            context_window: self.context_window,
            tools: self.tools,
            model_request: self.model_request,
            budget: self.budget,
            guardrails: self.guardrails,
        }))
    }

    fn validate(&self) -> Result<(), AgentBuildError> {
        if self.model.context_window_tokens() == 0 {
            return Err(AgentBuildError::ZeroContextWindow);
        }

        let capabilities = self.model.capabilities();
        if !self.tools.is_empty() && !capabilities.tool_calls {
            return Err(AgentBuildError::ToolCallsUnsupported);
        }

        match &self.model_request.tool_choice {
            ToolChoice::Required if self.tools.is_empty() => {
                return Err(AgentBuildError::RequiredToolChoiceWithoutTools);
            }
            ToolChoice::Named(name)
                if !self
                    .tools
                    .definitions()
                    .iter()
                    .any(|definition| definition.name == *name) =>
            {
                return Err(AgentBuildError::NamedToolChoiceNotRegistered { name: name.clone() });
            }
            ToolChoice::Auto | ToolChoice::None | ToolChoice::Required | ToolChoice::Named(_) => {}
        }
        let supported = match &self.model_request.tool_choice {
            ToolChoice::Auto if self.tools.is_empty() => true,
            ToolChoice::Auto => capabilities.tool_choice.auto,
            ToolChoice::None if self.tools.is_empty() => true,
            ToolChoice::None => capabilities.tool_choice.none,
            ToolChoice::Required => capabilities.tool_choice.required,
            ToolChoice::Named(_) => capabilities.tool_choice.named,
        };
        if !supported {
            return Err(AgentBuildError::ToolChoiceUnsupported);
        }

        if self.model_request.reasoning.is_some() && !capabilities.reasoning {
            return Err(AgentBuildError::ReasoningUnsupported);
        }

        Ok(())
    }
}
