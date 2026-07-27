//! Shell 能力桥接工具：把 [`ShellTool`] 包装为模型可见的类型化 [`Tool`]。
//!
//! 流式 chunk 桥接为 [`ToolOutputSink`]；聚合截断语义由能力层契约保证，
//! `max_output_bytes` 为构造参数，由 Runtime 装配时给定。

use std::{sync::Arc, time::Duration};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    capability::shell::{
        ShellOutcome, ShellOutputChannel, ShellOutputSink, ShellRequest, ShellTool, ShellToolError,
    },
    tool::{Tool, ToolContext, ToolError, ToolExecuteFuture, ToolOutputChannel, ToolOutputChunk},
};
use agent_types::ToolName;

/// `shell`：执行一条完整命令。
pub struct ShellExecTool {
    shell: Arc<dyn ShellTool>,
    max_output_bytes: u64,
}

/// `shell` 输入。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellInput {
    /// 完整命令；原样进入审计与确认。
    pub command: String,
    /// 工作目录；缺省为工作区根。
    pub workdir: Option<String>,
    /// 超时（毫秒）；缺省由实现侧给定。
    pub timeout_ms: Option<u64>,
}

impl ShellExecTool {
    /// 创建 Shell 桥接工具；`max_output_bytes` 为聚合输出上限（超限保留尾部）。
    pub fn new(shell: Arc<dyn ShellTool>, max_output_bytes: u64) -> Self {
        Self {
            shell,
            max_output_bytes,
        }
    }
}

impl Tool for ShellExecTool {
    type Input = ShellInput;
    type Output = ShellOutcome;

    fn name(&self) -> ToolName {
        ToolName::new("shell").expect("valid tool name")
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its aggregated output (stdout/stderr). stdin is \
         closed and background execution is not supported. Do NOT use this for file reading, \
         writing, editing or searching — use the dedicated file tools instead."
    }

    fn execute<'a>(
        &'a self,
        input: ShellInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, ShellOutcome> {
        let sink: ShellOutputSink = {
            let output_sink = context.output_sink.clone();
            Arc::new(move |chunk| {
                output_sink(ToolOutputChunk {
                    channel: match chunk.channel {
                        ShellOutputChannel::Stdout => ToolOutputChannel::Stdout,
                        ShellOutputChannel::Stderr => ToolOutputChannel::Stderr,
                    },
                    delta: chunk.data,
                });
            })
        };
        Box::pin(async move {
            self.shell
                .exec(
                    ShellRequest {
                        command: input.command,
                        workdir: input.workdir,
                        timeout: input.timeout_ms.map(Duration::from_millis),
                        max_output_bytes: Some(self.max_output_bytes),
                    },
                    sink,
                    context.cancellation,
                )
                .await
                .map_err(map_shell_error)
        })
    }
}

/// 能力错误到工具错误的映射；`Cancelled` 不进入模型可见结果——引擎在取消收敛时
/// 丢弃未完成的 dispatch，此映射仅为类型完备。
fn map_shell_error(error: ShellToolError) -> ToolError {
    match error {
        ShellToolError::InvalidInput { message } => ToolError::invalid_input(message),
        ShellToolError::Io { .. } | ShellToolError::Cancelled => {
            ToolError::execution(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{
        ShellOutputChunk,
        testutil::{MiniShell, block_on},
    };

    fn shell() -> Arc<MiniShell> {
        Arc::new(MiniShell {
            chunks: vec![
                ShellOutputChunk {
                    channel: ShellOutputChannel::Stdout,
                    data: "hello\n".to_owned(),
                },
                ShellOutputChunk {
                    channel: ShellOutputChannel::Stderr,
                    data: "warn\n".to_owned(),
                },
            ],
            exit_code: 0,
        })
    }

    fn input() -> ShellInput {
        ShellInput {
            command: "echo hello".to_owned(),
            workdir: None,
            timeout_ms: None,
        }
    }

    #[test]
    fn chunks_forward_to_tool_output_sink_with_channels() {
        let collected: Arc<Mutex<Vec<ToolOutputChunk>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_collected = collected.clone();
        let context = ToolContext::new(
            tokio_util::sync::CancellationToken::new(),
            Arc::new(move |chunk| sink_collected.lock().expect("lock").push(chunk)),
        );
        let tool = ShellExecTool::new(shell(), 1024);
        let outcome = block_on(tool.execute(input(), context)).expect("exec succeeds");

        assert_eq!(outcome.exit_code, Some(0));
        assert!(!outcome.truncated);
        assert_eq!(outcome.aggregated, "hello\nwarn\n");

        let collected = collected.lock().expect("lock");
        assert_eq!(
            collected.as_slice(),
            [
                ToolOutputChunk {
                    channel: ToolOutputChannel::Stdout,
                    delta: "hello\n".to_owned(),
                },
                ToolOutputChunk {
                    channel: ToolOutputChannel::Stderr,
                    delta: "warn\n".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn aggregated_output_keeps_tail_when_truncated() {
        let tool = ShellExecTool::new(shell(), 4);
        let outcome = block_on(tool.execute(input(), ToolContext::default())).expect("exec");
        assert_eq!(outcome.aggregated, "arn\n");
        assert!(outcome.truncated);
    }

    #[test]
    fn cancellation_maps_to_execution_error_without_reaching_output() {
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let tool = ShellExecTool::new(shell(), 1024);
        let error =
            block_on(tool.execute(input(), ToolContext::new(cancellation, Arc::new(|_| {}))))
                .expect_err("cancelled exec must fail");
        // 取消不进入模型可见结果的约定下，此映射仅为类型完备（引擎会丢弃该 dispatch）。
        assert!(matches!(error, ToolError::Execution { .. }));
    }
}
