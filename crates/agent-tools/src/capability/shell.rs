//! Shell 能力契约：一等工具，承载任意系统命令执行。
//!
//! 契约固化的实现侧义务（真实实现归 Runtime/Adapter）：
//!
//! - stdin 封闭，不支持交互式 TTY 与后台任务；`exec` 返回即进程树结束；
//! - 完整命令原样进入审计与确认，不在展示时截断或静默改写；
//! - 敏感环境变量（API Key、令牌等）默认不传给子进程，允许名单归实现侧配置；
//! - 超时与取消必须终止整棵进程树，不留孤儿进程。
//!
//! 结构化文件能力与 Shell 并存：模型读/写/搜文件应使用文件工具，Shell 不按
//! 每条系统命令枚举专用工具。

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Shell 执行的 Future。
pub type ShellFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ShellOutcome, ShellToolError>> + Send + 'a>>;

/// Shell 失败分类；非零退出码不是错误，属于 [`ShellOutcome`]。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ShellToolError {
    /// 请求参数违反契约（空命令等）。
    #[error("invalid input: {message}")]
    InvalidInput {
        /// 模型可读的失败原因。
        message: String,
    },
    /// 进程启动或底层 I/O 失败。
    #[error("io error: {message}")]
    Io {
        /// 模型可读的失败原因。
        message: String,
    },
    /// 执行被取消；取消语义由引擎在外围收敛，不进入模型可见结果。
    #[error("shell execution cancelled")]
    Cancelled,
}

/// Shell 输出通道。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellOutputChannel {
    /// 标准输出。
    Stdout,
    /// 标准错误。
    Stderr,
}

/// 一段流式输出。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellOutputChunk {
    /// 输出通道。
    pub channel: ShellOutputChannel,
    /// 文本片段。
    pub data: String,
}

/// 流式输出回调；实现侧按到达顺序回调。
pub type ShellOutputSink = Arc<dyn Fn(ShellOutputChunk) + Send + Sync>;

/// Shell 执行请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellRequest {
    /// 完整命令；原样进入审计与确认。
    pub command: String,
    /// 工作目录；缺省为能力根。
    pub workdir: Option<String>,
    /// 超时；缺省由实现侧给定。超时终止置 `timed_out`。
    pub timeout: Option<Duration>,
    /// 聚合输出字节上限；超限保留尾部并置 `truncated`。
    pub max_output_bytes: Option<u64>,
}

/// Shell 执行结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShellOutcome {
    /// 退出码；`None` 表示被信号终止或无法取得。
    pub exit_code: Option<i32>,
    /// 是否因超时终止。
    pub timed_out: bool,
    /// 按 chunk 到达顺序聚合的输出；超限保留尾部。
    pub aggregated: String,
    /// 聚合输出因超限被截断。
    pub truncated: bool,
}

/// Provider-neutral 的 Shell 能力。
pub trait ShellTool: Send + Sync {
    /// 执行一条完整命令，流式回报输出，返回聚合结果。
    fn exec<'a>(
        &'a self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> ShellFuture<'a>;
}

/// 契约级聚合截断：fake 与真实实现共用，保证尾部保留语义同构。
///
/// 按字节截断时对齐 char 边界（截断点可能略早于上限）。
pub fn tail_truncate(text: &str, max_bytes: Option<u64>) -> (String, bool) {
    let Some(max_bytes) = max_bytes else {
        return (text.to_owned(), false);
    };
    let max_bytes = max_bytes as usize;
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_truncate_keeps_tail_on_char_boundary() {
        let (text, truncated) = tail_truncate("abcdef", Some(3));
        assert_eq!(text, "def");
        assert!(truncated);

        let (text, truncated) = tail_truncate("中文测试abc", Some(7));
        assert_eq!(text, "试abc");
        assert!(truncated);

        let (text, truncated) = tail_truncate("abc", Some(10));
        assert_eq!(text, "abc");
        assert!(!truncated);

        let (text, truncated) = tail_truncate("abc", None);
        assert_eq!(text, "abc");
        assert!(!truncated);
    }
}
