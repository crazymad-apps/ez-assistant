use serde::{Deserialize, Serialize};

use crate::ProviderId;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// 实际生成 AssistantMessage 的模型身份。
pub struct ModelIdentity {
    /// 模型服务提供方，例如 `deepseek`。
    pub provider: ProviderId,
    /// Provider 侧的模型名称，例如 `deepseek-reasoner`。
    pub model: String,
}

impl ModelIdentity {
    /// 创建模型身份。
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
/// Provider 结束本次模型输出的原因。
pub enum FinishReason {
    /// 模型正常结束，通常表示已经给出最终回答。
    Stop,
    /// 模型请求调用工具，Agent Engine 后续需要执行工具并决定是否继续。
    ToolCalls,
    /// 达到模型输出长度限制。
    Length,
    /// Provider 的内容安全策略终止了输出。
    ContentFilter,
    /// 调用方主动取消了本次模型请求。
    Cancelled,
    /// 尚未纳入规范枚举的 Provider 原生结束原因。
    Other(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
/// 一次模型调用的规范 token 用量。
pub struct TokenUsage {
    /// 本次请求消耗的输入 token。
    pub input_tokens: u64,
    /// 本次响应产生的输出 token。
    pub output_tokens: u64,
    /// Provider 报告的总 token；保留原值，不在本类型中重新估算。
    pub total_tokens: u64,
    /// 输入 token 中命中缓存的数量；Provider 未提供时为 `None`。
    pub cached_input_tokens: Option<u64>,
    /// 输出 token 中 reasoning token 的数量；Provider 未提供时为 `None`。
    pub reasoning_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_reason_preserves_unknown_values() {
        let reason = FinishReason::Other("provider_specific".to_owned());
        let json = serde_json::to_string(&reason).expect("serialize reason");

        assert_eq!(
            serde_json::from_str::<FinishReason>(&json).expect("deserialize reason"),
            reason
        );
    }
}
