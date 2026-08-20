use agent_types::ToolName;
use thiserror::Error;

/// 构建会话级 [`crate::Agent`] 时发现的跨字段配置错误。
///
/// 错误只携带定位配置所需的稳定事实，不包含 Prompt、Provider Options、credential
/// 或完整工具 Schema。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum AgentBuildError {
    /// 模型服务没有声明有效的上下文窗口大小。
    #[error("model context window must be greater than zero")]
    ZeroContextWindow,
    /// 已注册工具，但模型没有声明 tool-call 能力。
    #[error("registered tools require a model with tool-call capability")]
    ToolCallsUnsupported,
    /// 当前精确模型路由不支持请求选择的 ToolChoice。
    #[error("selected tool choice is not supported by the model route")]
    ToolChoiceUnsupported,
    /// 请求要求模型调用工具，但冻结工具集为空。
    #[error("required tool choice needs at least one registered tool")]
    RequiredToolChoiceWithoutTools,
    /// 请求指定了冻结工具集中不存在的工具。
    #[error("named tool choice `{name}` is not registered")]
    NamedToolChoiceNotRegistered {
        /// 未命中的模型可见工具名。
        name: ToolName,
    },
    /// 请求启用了 reasoning，但模型没有声明 reasoning 能力。
    #[error("reasoning request requires a model with reasoning capability")]
    ReasoningUnsupported,
}
