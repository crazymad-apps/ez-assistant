//! 一次 Agent 执行的不可变规格。
//!
//! [`ExecutionSpec`] 是已经由 Runtime 解析完成的执行事实源：指令、模型服务、
//! 工具快照、资源预算与可选 Guardrail 在启动前冻结，Core 不再维护配置默认值或
//! 覆盖顺序。

use std::sync::Arc;

use agent_context::ContextWindowEvaluator;
use agent_model::ModelService;
use agent_tools::ToolSetSnapshot;

use crate::GuardrailConfig;

/// 一次 Agent 执行的不可变规格（事实源）。
///
/// 执行上下文组装保持纯机械投影
/// （instructions→system、输入快照→conversation、工具快照→tools）；共享
/// Evaluator 只负责每个 Model Step 前的窗口预检，不在 Core 内发起压缩。
pub struct ExecutionSpec {
    /// 系统指令列表，直接映射 `ModelRequest.system`。
    pub instructions: Vec<String>,
    /// 已绑定的模型服务实例（构造期含 endpoint/credential/model）。
    pub model: Arc<dyn ModelService>,
    /// 每个 Model Step 前使用的共享上下文窗口判断入口。
    pub context_window: Arc<ContextWindowEvaluator>,
    /// 执行期不可变的工具集快照；空快照是合法输入（最小可执行 Agent 纯文本收尾）。
    pub tools: ToolSetSnapshot,
    /// 显式资源预算；全 `Option`，Core 不注入隐藏上限。
    pub budget: ExecutionBudget,
    /// 可选 Guardrail 配置；`None` 表示整个 Guardrail 机制关闭。
    pub guardrails: Option<GuardrailConfig>,
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
            instructions: vec!["You are a helpful assistant.".to_owned()],
            model: Arc::new(NoopModel {
                capabilities: ModelCapabilities {
                    reasoning: false,
                    tool_calls: true,
                    streaming: true,
                },
            }),
            context_window: Arc::new(ContextWindowEvaluator::new(0.8).expect("valid threshold")),
            tools: ToolSetSnapshot::default(),
            budget: ExecutionBudget::default(),
            guardrails: None,
        };
        assert_eq!(spec.instructions.len(), 1);
        // 空快照合法：最小可执行 Agent 不含任何工具。
        assert!(spec.tools.is_empty());
        assert!(spec.model.capabilities().tool_calls);
    }
}
