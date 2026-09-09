//! 在线或固定参数编译后的模型能力；不包含静态模型目录。

use agent_model::{ToolChoiceCapabilities, ToolImageProjection};

pub use assistant_protocol::ReasoningEffortKey;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// 目录可交给协议 Adapter 的受限 effort 值。
pub enum ReasoningEffortWireValue {
    String(String),
    PositiveInteger(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一个已校验的 effort 选项。
pub struct ResolvedReasoningEffort {
    pub key: ReasoningEffortKey,
    pub label: String,
    pub wire_value: ReasoningEffortWireValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 已编译的 reasoning 能力；effort 为空表示模型有 thinking 但没有强度概念。
pub struct ResolvedReasoningCapability {
    pub mode: assistant_protocol::ModelReasoningMode,
    pub efforts: Vec<ResolvedReasoningEffort>,
    pub default_effort: Option<ReasoningEffortKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Runtime 唯一编译入口产生的模型能力事实。
pub struct ResolvedModelCapabilities {
    pub image_input: bool,
    pub reasoning: Option<ResolvedReasoningCapability>,
    pub tool_calls: bool,
    pub tool_image_projection: ToolImageProjection,
    pub tool_choice: ToolChoiceCapabilities,
    pub streaming: bool,
}

impl ResolvedModelCapabilities {
    /// 当前 OpenAI Chat Completions Adapter 的保守能力基线。
    pub fn conservative_openai_chat_completions() -> Self {
        Self {
            image_input: false,
            reasoning: None,
            tool_calls: true,
            tool_image_projection: ToolImageProjection::Unsupported,
            tool_choice: ToolChoiceCapabilities::auto_only(),
            streaming: true,
        }
    }

    /// 未命中精确 Responses 路由时的保守能力基线。
    pub fn conservative_openai_responses() -> Self {
        Self {
            image_input: false,
            reasoning: None,
            tool_calls: true,
            tool_image_projection: ToolImageProjection::Unsupported,
            tool_choice: ToolChoiceCapabilities::auto_only(),
            streaming: true,
        }
    }

    pub fn reasoning_enabled(&self) -> bool {
        self.reasoning.is_some()
    }
}
