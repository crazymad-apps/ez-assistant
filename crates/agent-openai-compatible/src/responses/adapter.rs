use std::collections::BTreeMap;

use agent_model::{ReasoningEffort, ToolChoiceCapabilities, ToolImageProjection};
use agent_types::{ProtocolId, ProviderId};
use serde_json::Value;

use crate::ToolSchemaDialect;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Responses `function_call_output.output` 的固定 wire 形状。
pub enum FunctionOutputShape {
    /// 只接受一个字符串。
    StringOnly,
    /// 接受 `input_text` / `input_image` 内容数组。
    ContentParts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NormalizedReasoningShape {
    SummaryWithItemId,
    PlainTextContent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OpaqueReasoningPolicy {
    None,
    PreserveEncryptedItem,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一条精确 Responses 路由的协议方言。
///
/// 字段保持 crate 私有；Runtime 只能从具名构造器出发，再注入静态目录已经验证的能力。
pub struct ResponsesProtocolAdapter {
    pub(crate) provider: ProviderId,
    pub(crate) protocol: ProtocolId,
    pub(crate) supports_temperature: bool,
    pub(crate) supports_top_p: bool,
    pub(crate) supports_stop: bool,
    pub(crate) supports_max_output_tokens: bool,
    pub(crate) reasoning_effort_values: BTreeMap<ReasoningEffort, Value>,
    pub(crate) tool_choice: ToolChoiceCapabilities,
    pub(crate) function_output_shape: FunctionOutputShape,
    pub(crate) tool_image_projection: ToolImageProjection,
    pub(crate) tool_schema_dialect: ToolSchemaDialect,
    pub(crate) normalized_reasoning_shape: NormalizedReasoningShape,
    pub(crate) include_encrypted_reasoning: bool,
    pub(crate) opaque_reasoning: OpaqueReasoningPolicy,
    pub(crate) route_fingerprint: Option<String>,
}

impl ResponsesProtocolAdapter {
    /// 未命中精确路由时使用的保守兼容方言：文本、流式、Auto function call，
    /// 不猜测图片函数结果、reasoning 续传或额外 ToolChoice。
    pub fn openai_compatible(provider: ProviderId) -> Self {
        Self {
            provider,
            protocol: ProtocolId::new("openai.responses").expect("static Responses protocol id"),
            supports_temperature: true,
            supports_top_p: true,
            supports_stop: false,
            supports_max_output_tokens: true,
            reasoning_effort_values: BTreeMap::new(),
            tool_choice: ToolChoiceCapabilities::auto_only(),
            function_output_shape: FunctionOutputShape::StringOnly,
            tool_image_projection: ToolImageProjection::Unsupported,
            tool_schema_dialect: ToolSchemaDialect::OpenAiFunctionSubset,
            normalized_reasoning_shape: NormalizedReasoningShape::SummaryWithItemId,
            include_encrypted_reasoning: false,
            opaque_reasoning: OpaqueReasoningPolicy::None,
            route_fingerprint: None,
        }
    }

    /// OpenAI 官方基础方言，仅用于官方 fixture 与显式配置；是否进入随包目录由 Runtime 决定。
    pub fn openai() -> Self {
        Self {
            function_output_shape: FunctionOutputShape::ContentParts,
            tool_choice: ToolChoiceCapabilities::all(),
            include_encrypted_reasoning: true,
            opaque_reasoning: OpaqueReasoningPolicy::PreserveEncryptedItem,
            ..Self::openai_compatible(ProviderId::new("openai").expect("static OpenAI provider id"))
        }
    }

    /// 已由本地真实验证锁定的 DeepSeek Responses thinking 方言。
    pub fn deepseek() -> Self {
        Self {
            supports_temperature: false,
            supports_top_p: false,
            normalized_reasoning_shape: NormalizedReasoningShape::PlainTextContent,
            include_encrypted_reasoning: true,
            opaque_reasoning: OpaqueReasoningPolicy::PreserveEncryptedItem,
            ..Self::openai_compatible(
                ProviderId::new("deepseek").expect("static DeepSeek provider id"),
            )
        }
    }

    /// 已由本地真实验证锁定的 DashScope Qwen Responses 方言。
    pub fn qwen() -> Self {
        Self {
            tool_image_projection: ToolImageProjection::AggregatedUserInput,
            ..Self::openai_compatible(
                ProviderId::new("dashscope").expect("static DashScope provider id"),
            )
        }
    }

    /// 已由本地真实验证锁定的 Kimi Code Responses 方言。
    pub fn kimi() -> Self {
        Self {
            function_output_shape: FunctionOutputShape::ContentParts,
            tool_image_projection: ToolImageProjection::NativeFunctionOutput,
            ..Self::openai_compatible(
                ProviderId::new("moonshot").expect("static Moonshot provider id"),
            )
        }
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn with_reasoning_efforts(mut self, values: BTreeMap<ReasoningEffort, Value>) -> Self {
        self.reasoning_effort_values = values;
        self
    }

    pub fn with_tool_choice(mut self, capabilities: ToolChoiceCapabilities) -> Self {
        self.tool_choice = capabilities;
        self
    }

    pub fn with_tool_image_projection(mut self, projection: ToolImageProjection) -> Self {
        self.tool_image_projection = projection;
        self
    }

    pub fn with_function_output_shape(mut self, shape: FunctionOutputShape) -> Self {
        self.function_output_shape = shape;
        self
    }

    pub(crate) fn bind_route(mut self, fingerprint: String) -> Self {
        self.route_fingerprint = Some(fingerprint);
        self
    }
}
