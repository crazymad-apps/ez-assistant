use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Error, serde::Serialize, serde::Deserialize)]
/// 单次模型调用的规范错误分类。
///
/// 错误只携带已经脱敏的可展示诊断信息：生产错误的 Adapter 不得把 credential、
/// 完整 prompt 或未脱敏的原始响应正文放进任何字段。底层错误的细节由 Adapter
/// 在转换时折进消息文本。
///
/// 建立前失败（配置、认证、连接等）由 `ModelService::stream` 返回 `Err`；
/// 流建立后的失败以 `ModelEvent::TurnFailed` 受控终态结束，两者语义不同。
pub enum ModelError {
    /// 调用方或 Runtime 提供的配置不合法。
    #[error("invalid model configuration: {0}")]
    Config(String),
    /// 认证失败。
    #[error("model authentication failed: {0}")]
    Auth(String),
    /// 底层连接、HTTP 或流帧传输失败。
    #[error("model transport failed: {0}")]
    Transport(String),
    /// Provider 明确拒绝了请求。
    #[error("provider rejected the request (status {status:?}): {message}")]
    Provider {
        /// 已脱敏的 Provider 诊断信息。
        message: String,
        /// Provider 的 HTTP 状态码；非 HTTP 协议为 `None`。
        status: Option<u16>,
    },
    /// 限流或暂时不可用。
    #[error("provider rate limited the request: {0}")]
    RateLimited(String),
    /// 响应不满足协议预期（schema、生命周期、畸形帧等）。
    #[error("provider stream violated the protocol: {0}")]
    Protocol(String),
    /// Tool arguments 分片无法组装成完整 JSON 值。
    #[error("tool call arguments could not be assembled: {0}")]
    ToolArguments(String),
    /// 调用方取消了本次调用。
    #[error("model call was cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_contains_only_sanitized_fields() {
        // Display 只允许渲染脱敏后的字段本身，不夹带任何额外上下文。
        let error = ModelError::Provider {
            message: "upstream rejected the request".to_owned(),
            status: Some(403),
        };
        assert_eq!(
            error.to_string(),
            "provider rejected the request (status Some(403)): upstream rejected the request"
        );
        assert_eq!(
            ModelError::Cancelled.to_string(),
            "model call was cancelled"
        );
        assert_eq!(
            ModelError::Protocol("missing terminal event".to_owned()).to_string(),
            "provider stream violated the protocol: missing terminal event"
        );
    }
}
