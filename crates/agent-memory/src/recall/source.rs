use std::{future::Future, pin::Pin};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    MemoryRecallError, MemoryRecallRequest, MemoryRecallResponse, RecallFailureKind,
    RecallSourceId, RecallSourceRequest, RecallSourceResponse,
};

/// 单个 RecallSource 操作返回的 boxed future。
pub type RecallSourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RecallSourceError>> + Send + 'a>>;

/// 统一 Memory Recall 操作返回的 boxed future。
pub type MemoryRecallFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MemoryRecallError>> + Send + 'a>>;

/// 单个 RecallSource 的稳定错误分类。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RecallSourceError {
    /// Source 当前不可用。
    #[error("recall source is unavailable: {message}")]
    Unavailable {
        /// 脱敏诊断。
        message: String,
    },
    /// Source 自身报告超时。
    #[error("recall source timed out: {message}")]
    Timeout {
        /// 脱敏诊断。
        message: String,
    },
    /// Source I/O 失败。
    #[error("recall source I/O failed: {message}")]
    Io {
        /// 脱敏诊断。
        message: String,
    },
    /// Source 读取到了不合法的数据。
    #[error("recall source returned invalid data: {message}")]
    InvalidData {
        /// 不包含原始数据的诊断。
        message: String,
    },
    /// Source 调用被取消。
    #[error("recall source was cancelled")]
    Cancelled,
    /// Source 内部失败，且没有更具体的稳定分类。
    #[error("recall source failed internally: {message}")]
    Internal {
        /// 脱敏诊断。
        message: String,
    },
}

impl RecallSourceError {
    pub(crate) fn into_failure_parts(self) -> (RecallFailureKind, String) {
        match self {
            Self::Unavailable { message } => (RecallFailureKind::Unavailable, message),
            Self::Timeout { message } => (RecallFailureKind::Timeout, message),
            Self::Io { message } => (RecallFailureKind::Io, message),
            Self::InvalidData { message } => (RecallFailureKind::InvalidData, message),
            Self::Cancelled => (
                RecallFailureKind::Cancelled,
                "source recall was cancelled".to_owned(),
            ),
            Self::Internal { message } => (RecallFailureKind::Internal, message),
        }
    }
}

/// 单一可检索记忆数据源的 Adapter 边界。
pub trait RecallSource: Send + Sync {
    /// 返回构造期固定、稳定且唯一的 Source ID。
    fn id(&self) -> &RecallSourceId;

    /// 使用 Source 自身的排序语义召回候选。
    fn recall(
        &self,
        request: RecallSourceRequest,
        cancellation: CancellationToken,
    ) -> RecallSourceFuture<'_, RecallSourceResponse>;
}

/// `recall_memory` 工具背后的统一召回能力。
pub trait MemoryRecall: Send + Sync {
    /// 执行一次可选择多个 Source 的召回。
    fn recall(
        &self,
        request: MemoryRecallRequest,
        cancellation: CancellationToken,
    ) -> MemoryRecallFuture<'_, MemoryRecallResponse>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_error_variants_map_to_stable_failure_categories() {
        let cases = [
            (
                RecallSourceError::Unavailable {
                    message: "unavailable".to_owned(),
                },
                RecallFailureKind::Unavailable,
            ),
            (
                RecallSourceError::Timeout {
                    message: "timeout".to_owned(),
                },
                RecallFailureKind::Timeout,
            ),
            (
                RecallSourceError::Io {
                    message: "io".to_owned(),
                },
                RecallFailureKind::Io,
            ),
            (
                RecallSourceError::InvalidData {
                    message: "invalid".to_owned(),
                },
                RecallFailureKind::InvalidData,
            ),
            (RecallSourceError::Cancelled, RecallFailureKind::Cancelled),
            (
                RecallSourceError::Internal {
                    message: "internal".to_owned(),
                },
                RecallFailureKind::Internal,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.into_failure_parts().0, expected);
        }
    }
}
