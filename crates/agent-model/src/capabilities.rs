use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
/// 一个模型服务对单次 Provider Turn 支持能力的显式声明。
///
/// 调用方据此决定请求内容（例如能力不含 tool call 时不下发工具定义），
/// Agent Core 不允许按 Provider 名称写特判逻辑。字段默认 `false`，
/// 未声明的能力视为不支持。
pub struct ModelCapabilities {
    /// 是否支持 reasoning/thinking 内容的产出与回传。
    pub reasoning: bool,
    /// 是否支持工具调用。
    pub tool_calls: bool,
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
            tool_calls: true,
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
                tool_calls: false,
                streaming: false,
            }
        );
    }
}
