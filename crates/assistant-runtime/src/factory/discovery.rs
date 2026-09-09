//! Host 在线发现契约与内部参数值；在线结果仅供当前视图或执行准备使用，不参与持久化。

use std::{error::Error, fmt, future::Future, pin::Pin, time::Duration};

/// 冻结的服务商连接输入。Endpoint 是 API 基础地址，不是模型 ID 或任意分页链接。
///
/// 凭据只借用于本次请求，不实现 Debug，也不读取全局环境或 Runtime Home。
pub struct ModelDiscoveryRequest<'a> {
    pub format: ModelDiscoveryFormat,
    /// 可选同源绝对路径覆盖；空字符串使用适配器默认值，不接受完整 URL、查询、编码转义或目录穿越。
    pub models_path: &'a str,
    pub endpoint: &'a str,
    pub api_key: &'a str,
    pub connect_timeout: Duration,
    /// 整次发现（含全部分页与响应体）的总时间上限。
    pub request_timeout: Duration,
}

pub use assistant_protocol::{
    DiscoveredModel, ModelDiscoveryFormat, ModelFeatureSupport, ModelParameters,
    ModelReasoningMode, ModelTokenLimit,
};

/// 单次发现 future；调用方丢弃它即取消本次获取，不留下后台分页任务。
pub type ModelDiscoveryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<DiscoveredModel>, ModelDiscoveryError>> + Send + 'a>>;

/// 调用方可据此展示重新配置、重试或不支持等动作，不解析底层错误文字。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelDiscoveryErrorKind {
    Unsupported,
    InvalidConfiguration,
    Authentication,
    RateLimited,
    Timeout,
    Unavailable,
    InvalidResponse,
    ResponseTooLarge,
}

/// 可安全展示的发现错误；底层 source 仅保留在内部错误链，不进入 Debug／Display。
pub struct ModelDiscoveryError {
    kind: ModelDiscoveryErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ModelDiscoveryError {
    pub fn new(kind: ModelDiscoveryErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn with_source(
        kind: ModelDiscoveryErrorKind,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    pub fn kind(&self) -> ModelDiscoveryErrorKind {
        self.kind
    }
}

impl fmt::Debug for ModelDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelDiscoveryError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ModelDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self.kind {
            ModelDiscoveryErrorKind::Unsupported => "model discovery is not supported",
            ModelDiscoveryErrorKind::InvalidConfiguration => {
                "invalid model discovery configuration"
            }
            ModelDiscoveryErrorKind::Authentication => "model discovery authentication failed",
            ModelDiscoveryErrorKind::RateLimited => "model discovery is rate limited",
            ModelDiscoveryErrorKind::Timeout => "model discovery timed out",
            ModelDiscoveryErrorKind::Unavailable => "model discovery is unavailable",
            ModelDiscoveryErrorKind::InvalidResponse => "invalid model discovery response",
            ModelDiscoveryErrorKind::ResponseTooLarge => "model discovery response exceeds limits",
        };
        formatter.write_str(message)
    }
}

impl Error for ModelDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_error_text_omits_sensitive_source_but_preserves_internal_cause() {
        let error = ModelDiscoveryError::with_source(
            ModelDiscoveryErrorKind::InvalidResponse,
            std::io::Error::other("fixture-secret-from-provider"),
        );
        assert!(!format!("{error} {error:?}").contains("fixture-secret"));
        assert_eq!(
            error.source().expect("internal cause").to_string(),
            "fixture-secret-from-provider"
        );
    }
}
