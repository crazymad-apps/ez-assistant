//! Shell 能力契约：完整 command、绝对工作目录、超时、分离输出与取消。
//!
//! 平台 launcher、环境过滤和真实进程树管理属于 Adapter；模型输入保持完整 command，
//! 不在本 crate 中解析 Shell AST，也不把 program/args 暴露给模型。

use std::{future::Future, num::NonZeroU64, pin::Pin, sync::Arc, time::Duration};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::AbsolutePath;

pub type ShellFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ShellOutcome, ShellToolError>> + Send + 'a>>;

/// Shell 策略读取的类型化 resolved 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellAuthorizationFacts {
    /// 完整命令字符串，不解析管道或重定向。
    pub command: String,
    /// 已解析的绝对逻辑工作目录。
    pub workdir: AbsolutePath,
    /// 已落实默认值并通过实例上限校验的超时。
    pub timeout: Duration,
    /// 工具返回后是否继续管理本次调用产生的进程树。
    pub process_mode: ShellProcessMode,
}

/// Shell 调用产生的进程树生命周期。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellProcessMode {
    /// 工具返回前清理仍可管理的进程树。
    #[default]
    Managed,
    /// 主 Shell 退出且输出管道收敛后，允许后代脱离工具生命周期。
    Detached,
}

/// Shell 失败分类；非零退出码不是错误，超时是保留部分输出的模型可见错误。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ShellToolError {
    #[error("invalid input: {message}")]
    InvalidInput { message: String },
    #[error("io error: {message}")]
    Io { message: String },
    #[error("shell execution timed out")]
    TimedOut {
        stdout: String,
        stderr: String,
        truncated: bool,
    },
    /// Adapter 完成进程树清理后的取消控制结果；Engine 不把它回喂模型。
    #[error("shell execution cancelled")]
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOutputChannel {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellOutputChunk {
    /// 输出来自 stdout 还是 stderr。
    pub channel: ShellOutputChannel,
    /// 本次增量文本。
    pub data: String,
}

pub type ShellOutputSink = Arc<dyn Fn(ShellOutputChunk) + Send + Sync>;

/// 已落实全部默认值和上限的 Shell 能力请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellRequest {
    /// 完整命令，原样交给平台 Shell launcher。
    pub command: String,
    pub workdir: AbsolutePath,
    pub timeout: Duration,
    /// stdout + stderr 合计允许保留的最大字节数。
    pub max_output_bytes: NonZeroU64,
    /// 本次命令采用的显式进程树生命周期。
    pub process_mode: ShellProcessMode,
}

/// 正常结束的 Shell 结果；非零 `exit_code` 仍属于成功结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShellOutcome {
    /// 正常退出码；被信号终止或平台无法提供时为 `None`。
    pub exit_code: Option<i32>,
    /// 在合计输出上限内保留的标准输出。
    pub stdout: String,
    /// 在合计输出上限内保留的标准错误输出。
    pub stderr: String,
    /// 是否有输出因上限而未被保留。
    pub truncated: bool,
    /// 实际执行采用的进程树生命周期。
    pub process_mode: ShellProcessMode,
}

pub trait ShellTool: Send + Sync {
    fn exec<'a>(
        &'a self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> ShellFuture<'a>;
}

/// 取不超过给定字节数的 UTF-8 前缀；边界落在字符内部时向前收缩。
pub fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_prefix_respects_bytes_and_char_boundaries() {
        assert_eq!(utf8_prefix("abcdef", 3), "abc");
        assert_eq!(utf8_prefix("中文abc", 4), "中");
        assert_eq!(utf8_prefix("abc", 10), "abc");
        assert_eq!(utf8_prefix("abc", 0), "");
    }

    #[test]
    fn timeout_and_nonzero_exit_have_distinct_contract_shapes() {
        let timeout = ShellToolError::TimedOut {
            stdout: "partial out".to_owned(),
            stderr: "partial err".to_owned(),
            truncated: true,
        };
        assert!(matches!(timeout, ShellToolError::TimedOut { .. }));

        let nonzero = ShellOutcome {
            exit_code: Some(7),
            stdout: String::new(),
            stderr: "failed command".to_owned(),
            truncated: false,
            process_mode: ShellProcessMode::Managed,
        };
        assert_eq!(nonzero.exit_code, Some(7));
    }
}
