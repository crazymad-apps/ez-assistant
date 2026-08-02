//! 脚本化 [`ShellTool`] Fake：回放 stdout/stderr、退出、超时、I/O 失败与取消。

use std::{collections::VecDeque, sync::Mutex};

use agent_tools::{
    ShellFuture, ShellOutcome, ShellOutputChannel, ShellOutputChunk, ShellRequest, ShellTool,
    ShellToolError, utf8_prefix,
};
use tokio_util::sync::CancellationToken;

/// 一条脚本回放完毕后的结算方式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FakeShellCompletion {
    /// 正常结束；非零退出码仍是成功结果。
    Exit { exit_code: Option<i32> },
    /// 超时结束；已回放的部分输出进入结构化超时错误。
    TimedOut,
    /// 模拟 launcher 或管道 I/O 失败。
    Io { message: String },
}

/// 一次脚本化的 Shell 执行行为。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeShellScript {
    /// 按到达顺序回放的输出 chunk。
    pub chunks: Vec<ShellOutputChunk>,
    pub completion: FakeShellCompletion,
}

impl FakeShellScript {
    /// 单段 stdout、退出码 0。
    pub fn stdout(text: impl Into<String>) -> Self {
        Self {
            chunks: vec![ShellOutputChunk {
                channel: ShellOutputChannel::Stdout,
                data: text.into(),
            }],
            completion: FakeShellCompletion::Exit { exit_code: Some(0) },
        }
    }

    /// 指定 stdout/stderr chunk 与退出码。
    pub fn exit(chunks: Vec<ShellOutputChunk>, exit_code: Option<i32>) -> Self {
        Self {
            chunks,
            completion: FakeShellCompletion::Exit { exit_code },
        }
    }

    /// 回放部分输出后按超时失败。
    pub fn timed_out(chunks: Vec<ShellOutputChunk>) -> Self {
        Self {
            chunks,
            completion: FakeShellCompletion::TimedOut,
        }
    }

    /// 不产生输出，直接模拟 I/O 失败。
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            chunks: Vec::new(),
            completion: FakeShellCompletion::Io {
                message: message.into(),
            },
        }
    }
}

/// 脚本化 Shell Fake；每次执行消费队首脚本，并记录已经落实默认值的请求。
pub struct FakeShellTool {
    scripts: Mutex<VecDeque<FakeShellScript>>,
    requests: Mutex<Vec<ShellRequest>>,
}

impl FakeShellTool {
    pub fn new(scripts: impl IntoIterator<Item = FakeShellScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 取出已收到的全部请求（按到达顺序）。
    pub fn take_requests(&self) -> Vec<ShellRequest> {
        std::mem::take(&mut self.requests.lock().expect("lock requests"))
    }
}

impl ShellTool for FakeShellTool {
    fn exec<'a>(
        &'a self,
        request: ShellRequest,
        sink: agent_tools::ShellOutputSink,
        cancellation: CancellationToken,
    ) -> ShellFuture<'a> {
        let script = self.scripts.lock().expect("lock scripts").pop_front();
        self.requests
            .lock()
            .expect("lock requests")
            .push(request.clone());
        Box::pin(async move {
            let Some(script) = script else {
                return Err(ShellToolError::InvalidInput {
                    message: "fake shell ran out of scripts".to_owned(),
                });
            };
            if request.command.trim().is_empty() {
                return Err(ShellToolError::InvalidInput {
                    message: "command must not be empty".to_owned(),
                });
            }
            if cancellation.is_cancelled() {
                return Err(ShellToolError::Cancelled);
            }

            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut retained_bytes = 0_usize;
            let maximum_bytes =
                usize::try_from(request.max_output_bytes.get()).unwrap_or(usize::MAX);
            let mut truncated = false;
            for chunk in script.chunks {
                if cancellation.is_cancelled() {
                    return Err(ShellToolError::Cancelled);
                }
                let remaining = maximum_bytes.saturating_sub(retained_bytes);
                let retained = utf8_prefix(&chunk.data, remaining);
                if retained.len() < chunk.data.len() {
                    truncated = true;
                }
                if retained.is_empty() {
                    continue;
                }
                retained_bytes += retained.len();
                let retained = retained.to_owned();
                match chunk.channel {
                    ShellOutputChannel::Stdout => stdout.push_str(&retained),
                    ShellOutputChannel::Stderr => stderr.push_str(&retained),
                }
                sink(ShellOutputChunk {
                    channel: chunk.channel,
                    data: retained,
                });
            }

            match script.completion {
                FakeShellCompletion::Exit { exit_code } => Ok(ShellOutcome {
                    exit_code,
                    stdout,
                    stderr,
                    truncated,
                    process_mode: request.process_mode,
                }),
                FakeShellCompletion::TimedOut => Err(ShellToolError::TimedOut {
                    stdout,
                    stderr,
                    truncated,
                }),
                FakeShellCompletion::Io { message } => Err(ShellToolError::Io { message }),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        num::NonZeroU64,
        sync::Arc,
        task::{Context, Poll, Waker},
        time::Duration,
    };

    use agent_tools::{AbsolutePath, SessionPathResolver};

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    fn workdir() -> AbsolutePath {
        AbsolutePath::new(std::env::temp_dir().join("agent-testkit-shell"))
            .expect("absolute temp path")
    }

    fn request(command: &str, max_output_bytes: u64) -> ShellRequest {
        ShellRequest {
            command: command.to_owned(),
            workdir: SessionPathResolver::new(workdir())
                .resolve(".")
                .expect("workdir"),
            timeout: Duration::from_secs(120),
            max_output_bytes: NonZeroU64::new(max_output_bytes).expect("non-zero"),
            process_mode: agent_tools::ShellProcessMode::Managed,
        }
    }

    #[test]
    fn scripts_preserve_channels_nonzero_exit_and_requests() {
        let fake = FakeShellTool::new([FakeShellScript::exit(
            vec![
                ShellOutputChunk {
                    channel: ShellOutputChannel::Stdout,
                    data: "out\n".to_owned(),
                },
                ShellOutputChunk {
                    channel: ShellOutputChannel::Stderr,
                    data: "err\n".to_owned(),
                },
            ],
            Some(7),
        )]);
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let sink_chunks = chunks.clone();
        let outcome = block_on(fake.exec(
            request("command", 1024),
            Arc::new(move |chunk| sink_chunks.lock().expect("lock chunks").push(chunk)),
            CancellationToken::new(),
        ))
        .expect("exec succeeds");
        assert_eq!(outcome.exit_code, Some(7));
        assert_eq!(outcome.stdout, "out\n");
        assert_eq!(outcome.stderr, "err\n");
        assert_eq!(chunks.lock().expect("lock chunks").len(), 2);
        assert_eq!(fake.take_requests()[0].timeout, Duration::from_secs(120));
    }

    #[test]
    fn truncation_keeps_prefix_and_timeout_keeps_partial_channels() {
        let fake = FakeShellTool::new([
            FakeShellScript::stdout("abcdef"),
            FakeShellScript::timed_out(vec![ShellOutputChunk {
                channel: ShellOutputChannel::Stderr,
                data: "partial".to_owned(),
            }]),
        ]);
        let outcome = block_on(fake.exec(
            request("first", 3),
            Arc::new(|_| {}),
            CancellationToken::new(),
        ))
        .expect("exec succeeds");
        assert_eq!(outcome.stdout, "abc");
        assert!(outcome.truncated);

        let error = block_on(fake.exec(
            request("second", 1024),
            Arc::new(|_| {}),
            CancellationToken::new(),
        ))
        .expect_err("times out");
        assert_eq!(
            error,
            ShellToolError::TimedOut {
                stdout: String::new(),
                stderr: "partial".to_owned(),
                truncated: false,
            }
        );
    }

    #[test]
    fn io_failure_and_cancellation_are_distinct() {
        let fake = FakeShellTool::new([
            FakeShellScript::io("launcher failed"),
            FakeShellScript::stdout("unreachable"),
        ]);
        let error = block_on(fake.exec(
            request("first", 1024),
            Arc::new(|_| {}),
            CancellationToken::new(),
        ))
        .expect_err("io failure");
        assert_eq!(
            error,
            ShellToolError::Io {
                message: "launcher failed".to_owned()
            }
        );

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = block_on(fake.exec(request("second", 1024), Arc::new(|_| {}), cancellation))
            .expect_err("cancelled");
        assert_eq!(error, ShellToolError::Cancelled);
    }
}
