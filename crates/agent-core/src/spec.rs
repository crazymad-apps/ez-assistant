//! 一次 Agent 执行的不可变规格。
//!
//! [`ExecutionSpec`] 是已经由 Runtime 解析完成的执行事实源：指令、模型服务、
//! 工具快照、资源预算与可选 Guardrail 在启动前冻结，Core 不再维护配置默认值或
//! 覆盖顺序。

use std::sync::Arc;

use agent_context::ContextWindowEvaluator;
use agent_model::{
    GenerationConfig, ModelService, ProviderOptions, ReasoningConfig, SystemPromptSnapshot,
};
use agent_tools::ToolSetSnapshot;
use agent_types::ToolChoice;

use crate::GuardrailConfig;

/// 一次 Agent 执行的不可变规格（事实源）。
///
/// 执行上下文组装保持纯机械投影
/// （system_prompt→system、输入快照→conversation、工具快照→tools，model_request
/// → tool_choice/generation/reasoning/provider_options）；共享 Evaluator 只负责每个
/// Model Step 前的窗口预检，不在 Core 内发起压缩。
#[derive(Clone)]
pub struct ExecutionSpec {
    /// 已完成渲染的冻结 System Prompt，直接映射 `ModelRequest.system`。
    pub system_prompt: SystemPromptSnapshot,
    /// 已绑定的模型服务实例（构造期含 endpoint/credential/model）。
    pub model: Arc<dyn ModelService>,
    /// 每个 Model Step 前使用的共享上下文窗口判断入口。
    pub context_window: Arc<ContextWindowEvaluator>,
    /// 执行期不可变的工具集快照；空快照是合法输入（最小可执行 Agent 纯文本收尾）。
    pub tools: ToolSetSnapshot,
    /// 每个 Model Step 原样复用的 Provider-neutral 请求配置。
    pub model_request: ModelRequestConfig,
    /// 显式资源预算；全 `Option`，Core 不注入隐藏上限。
    pub budget: ExecutionBudget,
    /// 可选 Guardrail 配置；`None` 表示整个 Guardrail 机制关闭。
    pub guardrails: Option<GuardrailConfig>,
}

/// 一次 AgentExecution 内每个 Model Step 复用的模型请求配置。
///
/// 本类型只包含 [`agent_model::ModelRequest`] 已有的语义字段；endpoint、credential、
/// model 和 context window 仍属于 [`ModelService`] 的构造期配置。Provider 私有选项继续
/// 按命名空间隔离，由具体 Adapter 校验和解释。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelRequestConfig {
    /// 工具选择策略。
    pub tool_choice: ToolChoice,
    /// generation 参数；各字段为 `None` 时沿用 Provider 默认值。
    pub generation: GenerationConfig,
    /// reasoning 参数；`None` 表示本次请求不显式启用 reasoning。
    pub reasoning: Option<ReasoningConfig>,
    /// 命名空间隔离的 Provider 私有选项。
    pub provider_options: ProviderOptions,
}

impl Default for ModelRequestConfig {
    fn default() -> Self {
        Self {
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options: ProviderOptions::new(),
        }
    }
}

/// 单次执行的资源预算；预算是副作用前的硬边界。
///
/// - `max_steps`：模型 Turn 数上限，在每次**模型调用前**预检；
/// - `max_tool_calls`：实际 dispatch 的工具调用数上限，在每次 **dispatch 前**预检。
///
/// 到达上限即受控终止（`ExecutionFailed{BudgetExceeded}`）；批次内未执行的
/// 调用先结算错误 `ToolResult`，保证 Tool Call/Result 配对。
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionBudget {
    /// 模型 Turn 数上限；`None` 表示不限制。
    pub max_steps: Option<u32>,
    /// 实际 dispatch 的工具调用数上限；`None` 表示不限制。
    pub max_tool_calls: Option<u32>,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use agent_model::{
        ModelCallContext, ModelCapabilities, ModelError, ModelRequest, ModelStreamFuture,
    };

    use super::*;

    /// 最小的模型服务实现，只用于证明 ExecutionSpec 的装配形状。
    struct NoopModel {
        capabilities: ModelCapabilities,
    }

    impl ModelService for NoopModel {
        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn context_window_tokens(&self) -> u64 {
            128_000
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _context: ModelCallContext,
        ) -> ModelStreamFuture<'_> {
            Box::pin(std::future::ready(Err(ModelError::Config(
                "noop model never establishes a stream".to_owned(),
            ))))
        }
    }

    #[test]
    fn budget_defaults_to_no_hidden_limits() {
        let budget = ExecutionBudget::default();
        assert_eq!(budget.max_steps, None);
        assert_eq!(budget.max_tool_calls, None);
    }

    #[test]
    fn model_request_config_defaults_preserve_existing_core_behavior() {
        let config = ModelRequestConfig::default();
        assert_eq!(config.tool_choice, ToolChoice::Auto);
        assert_eq!(config.generation, GenerationConfig::default());
        assert_eq!(config.reasoning, None);
        assert!(config.provider_options.is_empty());
    }

    #[test]
    fn model_request_config_round_trips_serde() {
        let mut provider_options = ProviderOptions::new();
        provider_options
            .insert(
                "deepseek",
                serde_json::json!({"thinking": {"type": "enabled"}}),
            )
            .expect("valid static provider options");
        let config = ModelRequestConfig {
            tool_choice: ToolChoice::None,
            generation: GenerationConfig {
                temperature: Some(0.2),
                top_p: Some(0.9),
                max_output_tokens: Some(2_048),
                stop: vec!["stop".to_owned()],
            },
            reasoning: Some(ReasoningConfig { effort: None }),
            provider_options,
        };

        let json = serde_json::to_string(&config).expect("serialize request config");
        assert_eq!(
            serde_json::from_str::<ModelRequestConfig>(&json).expect("deserialize request config"),
            config
        );
    }

    #[test]
    fn budget_round_trips_serde() {
        let budget = ExecutionBudget {
            max_steps: Some(8),
            max_tool_calls: Some(32),
        };
        let json = serde_json::to_string(&budget).expect("serialize budget");
        assert_eq!(
            serde_json::from_str::<ExecutionBudget>(&json).expect("deserialize budget"),
            budget
        );
    }

    #[test]
    fn guardrails_have_no_hidden_defaults_and_round_trip() {
        let disabled = GuardrailConfig::default();
        assert_eq!(disabled.repeated_invocation, None);
        assert_eq!(disabled.consecutive_failures, None);

        let configured = GuardrailConfig {
            repeated_invocation: Some(crate::GuardrailCheckConfig {
                mode: crate::ActiveGuardrailMode::Observe,
                threshold: NonZeroU32::new(3).expect("non-zero threshold"),
            }),
            consecutive_failures: Some(crate::GuardrailCheckConfig {
                mode: crate::ActiveGuardrailMode::Enforce,
                threshold: NonZeroU32::new(5).expect("non-zero threshold"),
            }),
        };
        let json = serde_json::to_string(&configured).expect("serialize guardrails");
        assert_eq!(
            serde_json::from_str::<GuardrailConfig>(&json).expect("deserialize guardrails"),
            configured
        );
    }

    #[test]
    fn spec_assembles_with_empty_tool_snapshot() {
        let spec = ExecutionSpec {
            system_prompt: SystemPromptSnapshot::new(vec![
                "You are a helpful assistant.".to_owned(),
            ]),
            model: Arc::new(NoopModel {
                capabilities: ModelCapabilities {
                    reasoning: false,
                    tool_calls: true,
                    streaming: true,
                },
            }),
            context_window: Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
            tools: ToolSetSnapshot::default(),
            model_request: ModelRequestConfig::default(),
            budget: ExecutionBudget::default(),
            guardrails: None,
        };
        assert_eq!(spec.system_prompt.parts().len(), 1);
        // 空快照合法：最小可执行 Agent 不含任何工具。
        assert!(spec.tools.is_empty());
        assert!(spec.model.capabilities().tool_calls);

        let cloned = spec.clone();
        assert!(Arc::ptr_eq(&spec.model, &cloned.model));
        assert!(Arc::ptr_eq(&spec.context_window, &cloned.context_window));
        assert_eq!(spec.system_prompt, cloned.system_prompt);
        assert_eq!(spec.model_request, cloned.model_request);
    }
}
