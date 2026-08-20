use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// 工具结果中的图片如何投影到当前精确协议路由。
pub enum ToolImageProjection {
    #[default]
    Unsupported,
    NativeFunctionOutput,
    AggregatedUserInput,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// 当前精确协议路由支持的规范 ToolChoice 集合。
pub struct ToolChoiceCapabilities {
    pub auto: bool,
    pub none: bool,
    pub required: bool,
    pub named: bool,
}

impl ToolChoiceCapabilities {
    pub const fn auto_only() -> Self {
        Self {
            auto: true,
            none: false,
            required: false,
            named: false,
        }
    }

    pub const fn all() -> Self {
        Self {
            auto: true,
            none: true,
            required: true,
            named: true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// 一个模型服务对单次 Provider Turn 支持能力的显式声明。
///
/// 调用方据此决定请求内容（例如能力不含 tool call 时不下发工具定义），
/// Agent Core 不允许按 Provider 名称写特判逻辑。字段默认 `false`，
/// 未声明的能力视为不支持。
pub struct ModelCapabilities {
    /// 是否支持 reasoning/thinking 内容的产出与回传。
    pub reasoning: bool,
    /// 是否支持规范文件引用投影为原生图片输入。
    pub image_input: bool,
    /// 是否支持工具调用。
    pub tool_calls: bool,
    /// 是否能把规范 Tool Result Image Part 投影给模型。
    pub multimodal_tool_result: bool,
    /// 支持的 ToolChoice 集合。
    pub tool_choice: ToolChoiceCapabilities,
    /// 是否支持流式事件输出。
    pub streaming: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_round_trip() {
        let capabilities = ModelCapabilities {
            reasoning: true,
            image_input: true,
            tool_calls: true,
            multimodal_tool_result: true,
            tool_choice: ToolChoiceCapabilities::auto_only(),
            streaming: true,
        };
        let json = serde_json::to_string(&capabilities).expect("serialize capabilities");
        assert_eq!(
            serde_json::from_str::<ModelCapabilities>(&json).expect("deserialize capabilities"),
            capabilities
        );
    }

    #[test]
    fn capabilities_default_to_unsupported() {
        assert_eq!(
            ModelCapabilities::default(),
            ModelCapabilities {
                reasoning: false,
                image_input: false,
                tool_calls: false,
                multimodal_tool_result: false,
                tool_choice: ToolChoiceCapabilities::default(),
                streaming: false,
            }
        );
    }
}
