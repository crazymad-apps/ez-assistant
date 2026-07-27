//! 脚本化 [`ShellTool`] Fake：回放预定 chunk 流、退出码与超时标记，观察取消。

use std::{collections::VecDeque, sync::Mutex};

use agent_tools::{
    ShellFuture, ShellOutcome, ShellOutputChunk, ShellRequest, ShellTool, ShellToolError,
    tail_truncate,
};
use tokio_util::sync::CancellationToken;

/// 一次脚本化的 Shell 执行行为。
#[derive(Clone, Debug)]
pub struct FakeShellScript {
    /// 按到达顺序回放的输出 chunk。
    pub chunks: Vec<ShellOutputChunk>,
    /// 退出码；`None` 表示被信号终止或无法取得。
    pub exit_code: Option<i32>,
    /// 是否按超时终止结算。
    pub timed_out: bool,
}

impl FakeShellScript {
    /// 便捷构造：单段 stdout、退出码 0。
    pub fn stdout(text: &str) -> Self {
        Self {
            chunks: vec![ShellOutputChunk {
                channel: agent_tools::ShellOutputChannel::Stdout,
                data: text.to_owned(),
            }],
            exit_code: Some(0),
            timed_out: false,
        }
    }
}

/// 脚本化 Shell Fake。
///
/// 每次 `exec` 按顺序弹出一条脚本；脚本耗尽时以 `InvalidInput` 失败，提醒测试
/// 补脚本。请求按到达顺序记录，可用 [`Self::take_requests`] 取出断言。
pub struct FakeShellTool {
    scripts: Mutex<VecDeque<FakeShellScript>>,
    requests: Mutex<Vec<ShellRequest>>,
}

impl FakeShellTool {
    /// 用脚本队列创建 Fake。
    pub fn new(scripts: impl IntoIterator<Item = FakeShellScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 取出已收到的全部请求（按到达顺序），用于断言调用方行为。
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
        let script = {
            let mut scripts = self.scripts.lock().expect("lock scripts");
            scripts.pop_front()
        };
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
            if request.command.is_empty() {
                return Err(ShellToolError::InvalidInput {
                    message: "command must not be empty".to_owned(),
                });
            }
            let mut aggregated = String::new();
            for chunk in &script.chunks {
                if cancellation.is_cancelled() {
                    return Err(ShellToolError::Cancelled);
                }
                sink(chunk.clone());
                aggregated.push_str(&chunk.data);
            }
            let (aggregated, truncated) = tail_truncate(&aggregated, request.max_output_bytes);
            Ok(ShellOutcome {
                exit_code: script.exit_code,
                timed_out: script.timed_out,
                aggregated,
                truncated,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };

    use agent_tools::ShellOutputChannel;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("test future must never pend"),
        }
    }

    fn request(command: &str) -> ShellRequest {
        ShellRequest {
            command: command.to_owned(),
            workdir: None,
            timeout: None,
            max_output_bytes: None,
        }
    }

    #[test]
    fn scripts_replay_chunks_and_record_requests() {
        let fake = FakeShellTool::new([FakeShellScript::stdout("hello\n")]);
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_chunks = chunks.clone();
        let outcome = block_on(fake.exec(
            request("echo hello"),
            std::sync::Arc::new(move |chunk| {
                sink_chunks.lock().expect("lock").push(chunk);
            }),
            CancellationToken::new(),
        ))
        .expect("exec succeeds");
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.aggregated, "hello\n");
        assert!(!outcome.truncated);
        assert_eq!(chunks.lock().expect("lock").len(), 1);

        let requests = fake.take_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].command, "echo hello");

        // 脚本耗尽：受控失败。
        let error = block_on(fake.exec(
            request("echo again"),
            std::sync::Arc::new(|_| {}),
            CancellationToken::new(),
        ))
        .expect_err("out of scripts must fail");
        assert!(matches!(error, ShellToolError::InvalidInput { .. }));
    }

    #[test]
    fn truncation_keeps_tail_and_cancellation_aborts_drain() {
        let fake = FakeShellTool::new([
            FakeShellScript::stdout("abcdef"),
            FakeShellScript::stdout("abcdef"),
        ]);
        let mut truncated_request = request("first");
        truncated_request.max_output_bytes = Some(3);
        let outcome = block_on(fake.exec(
            truncated_request,
            std::sync::Arc::new(|_| {}),
            CancellationToken::new(),
        ))
        .expect("exec succeeds");
        assert_eq!(outcome.aggregated, "def");
        assert!(outcome.truncated);

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error =
            block_on(fake.exec(request("second"), std::sync::Arc::new(|_| {}), cancellation))
                .expect_err("cancelled exec must fail");
        assert_eq!(error, ShellToolError::Cancelled);
    }

    #[test]
    fn stderr_chunks_keep_channel() {
        let script = FakeShellScript {
            chunks: vec![ShellOutputChunk {
                channel: ShellOutputChannel::Stderr,
                data: "warn\n".to_owned(),
            }],
            exit_code: Some(1),
            timed_out: false,
        };
        let fake = FakeShellTool::new([script]);
        let outcome = block_on(fake.exec(
            request("cmd"),
            std::sync::Arc::new(|_| {}),
            CancellationToken::new(),
        ))
        .expect("exec succeeds");
        assert_eq!(outcome.exit_code, Some(1));
        assert_eq!(outcome.aggregated, "warn\n");
    }
}
