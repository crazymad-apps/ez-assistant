use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
/// 模型调用在 Transport 边界失败时保留下来的稳定分类。
///
/// 该分类只表达已经发生的事实，不直接声明错误是否应该重试。是否重试还取决于
/// 错误发生在建流前还是流建立后，以及上层显式装配的重试策略。
pub enum ModelTransportErrorKind {
    /// DNS、TLS、拒绝连接等响应建立前失败。
    Connection,
    /// 连接或整体请求超时。
    Timeout,
    /// 响应正文已经开始后连接中断。
    Interrupted,
}

impl std::fmt::Display for ModelTransportErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Connection => "connection",
            Self::Timeout => "timeout",
            Self::Interrupted => "interrupted",
        };
        formatter.write_str(value)
    }
}

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
    /// 底层连接、超时或响应流中断。
    #[error("model transport failed ({kind}): {message}")]
    Transport {
        /// 不依赖展示文本的稳定传输分类。
        kind: ModelTransportErrorKind,
        /// 已脱敏的传输诊断信息。
        message: String,
    },
    /// Provider 明确拒绝了请求。
    #[error("provider rejected the request (status {status:?}): {message}")]
    Provider {
        /// 已脱敏的 Provider 诊断信息。
        message: String,
        /// Provider 的 HTTP 状态码；非 HTTP 协议为 `None`。
        status: Option<u16>,
    },
    /// Provider 对当前请求实施限流。
    #[error("provider rate limited the request: {message}")]
    RateLimited {
        /// 已脱敏的 Provider 诊断信息。
        message: String,
        /// Provider 明确给出的建议等待毫秒数。
        retry_after_ms: Option<u64>,
    },
    /// Provider 当前暂时不可用。
    #[error("provider temporarily unavailable (status {status:?}): {message}")]
    Unavailable {
        /// 已脱敏的 Provider 诊断信息。
        message: String,
        /// Provider 的 HTTP 状态码；非 HTTP 协议为 `None`。
        status: Option<u16>,
        /// Provider 明确给出的建议等待毫秒数。
        retry_after_ms: Option<u64>,
    },
    /// Provider 明确报告请求超过模型上下文窗口。
    #[error("model context window exceeded: {message}")]
    ContextOverflow {
        /// 已脱敏的 Provider 诊断信息。
        message: String,
    },
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
        assert_eq!(
            ModelError::ContextOverflow {
                message: "request exceeds the configured context window".to_owned(),
            }
            .to_string(),
            "model context window exceeded: request exceeds the configured context window"
        );
        assert_eq!(
            ModelError::Transport {
                kind: ModelTransportErrorKind::Timeout,
                message: "request timed out".to_owned(),
            }
            .to_string(),
            "model transport failed (timeout): request timed out"
        );
        assert_eq!(
            ModelError::Unavailable {
                message: "service overloaded".to_owned(),
                status: Some(503),
                retry_after_ms: Some(2_000),
            }
            .to_string(),
            "provider temporarily unavailable (status Some(503)): service overloaded"
        );
    }

    #[test]
    fn structured_errors_round_trip_through_json() {
        let errors = [
            ModelError::Transport {
                kind: ModelTransportErrorKind::Connection,
                message: "connection refused".to_owned(),
            },
            ModelError::RateLimited {
                message: "slow down".to_owned(),
                retry_after_ms: Some(1_000),
            },
            ModelError::Unavailable {
                message: "try later".to_owned(),
                status: Some(503),
                retry_after_ms: None,
            },
        ];

        for error in errors {
            let json = serde_json::to_string(&error).expect("error should serialize");
            let decoded: ModelError =
                serde_json::from_str(&json).expect("error should deserialize");
            assert_eq!(decoded, error);
        }
    }
}
