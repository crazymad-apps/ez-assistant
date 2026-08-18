use std::{error::Error, fmt, future::Future, pin::Pin};

/// Runtime Store 异步操作的统一 Future。
pub type StoreFuture<'a, Output> =
    Pin<Box<dyn Future<Output = Result<Output, StoreError>> + Send + 'a>>;

/// 调用方可以据此选择重试、隔离 Session 或停止 Runtime 的稳定错误分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreErrorKind {
    /// 存储 worker 已关闭、失联或无法提供服务。
    Unavailable,
    /// 持久化内容无法解析或破坏领域不变量。
    InvalidData,
    /// 当前持久化状态与命令前置条件冲突。
    Conflict,
    /// 调用方提供的存储命令不满足边界约束。
    InvalidInput,
    /// Store 管理的外部资源当前不存在或不可访问。
    ResourceUnavailable,
    /// 本地 I/O 或数据库操作失败。
    Internal,
}

/// Runtime Store 失败；Display 只包含安全稳定信息，具体 source 留在进程内诊断。
#[derive(Debug)]
pub struct StoreError {
    kind: StoreErrorKind,
    message: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl StoreError {
    /// 创建不带底层 source 的安全存储错误。
    pub fn new(kind: StoreErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            source: None,
        }
    }

    /// 创建带进程内诊断 source 的安全存储错误。
    pub fn with_source(
        kind: StoreErrorKind,
        message: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message,
            source: Some(Box::new(source)),
        }
    }

    /// 返回稳定错误分类。
    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    /// 返回不包含路径、正文或数据库细节的安全消息。
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixtureSource;

    impl fmt::Display for FixtureSource {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("private path /tmp/fixture")
        }
    }

    impl Error for FixtureSource {}

    #[test]
    fn display_is_safe_while_source_remains_available_in_process() {
        let error = StoreError::with_source(
            StoreErrorKind::Internal,
            "runtime storage operation failed",
            FixtureSource,
        );

        assert_eq!(error.to_string(), "runtime storage operation failed");
        assert_eq!(error.kind(), StoreErrorKind::Internal);
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("private path /tmp/fixture")
        );
    }
}
