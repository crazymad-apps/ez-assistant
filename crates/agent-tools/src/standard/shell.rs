//! 标准 Shell 工具壳：把模型输入解析为完整命令、绝对工作目录和有效执行限制。
//!
//! resolve 阶段只做纯计算，不解析 Shell AST，也不启动进程；真正的 Shell launcher、
//! 环境过滤、超时和进程树清理由注入的 [`ShellTool`] 实现负责。

use std::{num::NonZeroU64, sync::Arc, time::Duration};

use agent_types::ToolName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    SessionPathResolver, ToolResolution,
    capability::shell::{
        ShellAuthorizationFacts, ShellOutcome, ShellOutputChannel, ShellOutputSink,
        ShellProcessMode, ShellRequest, ShellTool, ShellToolError,
    },
    tool::{
        Tool, ToolContext, ToolError, ToolExecuteFuture, ToolInputDefaults, ToolOutputChannel,
        ToolOutputChunk,
    },
};

use super::ToolConfigurationError;

/// `shell` 的有效默认超时、允许的最大超时和输出保留上限。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellExecToolConfig {
    default_timeout: Duration,
    maximum_timeout: Duration,
    max_output_bytes: NonZeroU64,
}

impl ShellExecToolConfig {
    /// 创建 Shell 工具实例配置；超时必须使用完整毫秒且默认值不得超过最大值。
    pub fn new(
        default_timeout: Duration,
        maximum_timeout: Duration,
        max_output_bytes: NonZeroU64,
    ) -> Result<Self, ToolConfigurationError> {
        if default_timeout.as_millis() == 0 {
            return Err(ToolConfigurationError::new(
                "shell default_timeout must be at least one millisecond",
            ));
        }
        if maximum_timeout.as_millis() == 0 {
            return Err(ToolConfigurationError::new(
                "shell maximum_timeout must be at least one millisecond",
            ));
        }
        if default_timeout > maximum_timeout {
            return Err(ToolConfigurationError::new(
                "shell default_timeout must not exceed maximum_timeout",
            ));
        }
        if default_timeout.as_millis() > u64::MAX as u128
            || maximum_timeout.as_millis() > u64::MAX as u128
        {
            return Err(ToolConfigurationError::new(
                "shell timeout must fit in an unsigned 64-bit millisecond value",
            ));
        }
        if Duration::from_millis(default_timeout.as_millis() as u64) != default_timeout
            || Duration::from_millis(maximum_timeout.as_millis() as u64) != maximum_timeout
        {
            return Err(ToolConfigurationError::new(
                "shell timeout must use whole milliseconds",
            ));
        }
        Ok(Self {
            default_timeout,
            maximum_timeout,
            max_output_bytes,
        })
    }

    /// 模型省略 `timeout_ms` 时采用的超时。
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// 单次模型调用允许请求的最大超时。
    pub fn maximum_timeout(&self) -> Duration {
        self.maximum_timeout
    }

    /// stdout 与 stderr 合计允许保留的最大字节数。
    pub fn max_output_bytes(&self) -> NonZeroU64 {
        self.max_output_bytes
    }
}

/// `shell`：执行一条完整命令。
pub struct ShellExecTool {
    shell: Arc<dyn ShellTool>,
    resolver: SessionPathResolver,
    config: ShellExecToolConfig,
}

/// 模型可见输入。可选项的默认值会同时出现在工具 Schema 中。
#[derive(Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellInput {
    /// 完整命令；可以包含管道、重定向等平台 Shell 语法。
    pub command: String,
    /// 绝对路径或相对 session_workdir 的工作目录。
    pub workdir: Option<String>,
    /// 执行超时（毫秒）。
    pub timeout_ms: Option<NonZeroU64>,
    /// 进程树生命周期；省略时使用 managed。
    pub process_mode: Option<ShellProcessMode>,
}

/// resolve 后冻结的 Shell 请求；执行和授权读取同一份有效值。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedShellInput {
    /// 未经 AST 解析、原样交给平台 launcher 的完整命令。
    pub command: String,
    /// 已完成词法归一化的绝对逻辑工作目录。
    pub workdir: crate::AbsolutePath,
    /// 已落实默认值并通过上限校验的毫秒超时。
    pub timeout_ms: NonZeroU64,
    /// 由宿主配置、模型不能覆盖的输出保留上限。
    pub max_output_bytes: NonZeroU64,
    /// 已落实默认值的进程树生命周期。
    pub process_mode: ShellProcessMode,
}

impl ShellExecTool {
    /// 用 Shell 能力、Session 路径解析器和实例限制装配标准工具壳。
    pub fn new(
        shell: Arc<dyn ShellTool>,
        resolver: SessionPathResolver,
        config: ShellExecToolConfig,
    ) -> Self {
        Self {
            shell,
            resolver,
            config,
        }
    }

    fn default_timeout_ms(&self) -> NonZeroU64 {
        NonZeroU64::new(self.config.default_timeout.as_millis() as u64)
            .expect("validated non-zero timeout")
    }
}

impl Tool for ShellExecTool {
    type Input = ShellInput;
    type ResolvedInput = ResolvedShellInput;
    type Output = ShellOutcome;

    fn name(&self) -> ToolName {
        ToolName::new("shell").expect("valid tool name")
    }

    fn description(&self) -> String {
        format!(
            "Execute a complete shell command and return stdout and stderr separately, retaining \
             at most {} bytes in total. stdin is closed. process_mode defaults to managed; use \
             detached only for fire-and-forget commands whose stdio is redirected. Detached \
             processes are not stopped by later run cancellation or session reset. Use the \
             dedicated file tools for file operations.",
            self.config.max_output_bytes
        )
    }

    fn input_defaults(&self) -> ToolInputDefaults {
        ToolInputDefaults::new()
            .with("workdir", self.resolver.session_workdir())
            .with("timeout_ms", self.default_timeout_ms())
            .with("process_mode", ShellProcessMode::Managed)
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        if input.command.trim().is_empty() {
            return Err(ToolError::invalid_input("command must not be empty"));
        }
        let workdir = match input.workdir {
            Some(workdir) => self
                .resolver
                .resolve(&workdir)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?,
            None => self.resolver.session_workdir().clone(),
        };
        let timeout_ms = input
            .timeout_ms
            .unwrap_or_else(|| self.default_timeout_ms());
        let timeout = Duration::from_millis(timeout_ms.get());
        if timeout > self.config.maximum_timeout {
            return Err(ToolError::invalid_input(format!(
                "timeout_ms must not exceed {}",
                self.config.maximum_timeout.as_millis()
            )));
        }

        let resolved = ResolvedShellInput {
            command: input.command,
            workdir: workdir.clone(),
            timeout_ms,
            max_output_bytes: self.config.max_output_bytes,
            process_mode: input.process_mode.unwrap_or_default(),
        };
        let command = resolved.command.clone();
        let process_mode = resolved.process_mode;
        let semantic_arguments = json!({
            "command": &command,
            "workdir": &resolved.workdir,
            "process_mode": process_mode,
        });
        Ok(ToolResolution::with_facts(
            resolved,
            ShellAuthorizationFacts {
                command,
                workdir,
                timeout,
                process_mode,
            },
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
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
                        timeout: Duration::from_millis(input.timeout_ms.get()),
                        max_output_bytes: input.max_output_bytes,
                        process_mode: input.process_mode,
                    },
                    sink,
                    context.cancellation,
                )
                .await
                .map_err(map_shell_error)
        })
    }
}

fn map_shell_error(error: ShellToolError) -> ToolError {
    match error {
        ShellToolError::InvalidInput { message } => ToolError::invalid_input(message),
        ShellToolError::TimedOut {
            stdout,
            stderr,
            truncated,
        } => ToolError::execution_with_details(
            "shell execution timed out",
            json!({
                "type": "timeout",
                "stdout": stdout,
                "stderr": stderr,
                "truncated": truncated,
            }),
        ),
        ShellToolError::Io { message } => ToolError::execution(message),
        // Engine 在执行取消后会直接收敛到 Cancelled，不会把这条文本回喂模型；
        // 此分支只为 ShellToolError 到 ToolError 的类型映射保持完备。
        ShellToolError::Cancelled => ToolError::execution("shell execution cancelled"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{
        Dispatcher, ResolvedBatchItemRef, ShellFuture, ShellOutputChunk, ToolRegistry,
        testutil::{block_on, tool_call},
    };

    struct ProbeShell {
        result: Result<ShellOutcome, ShellToolError>,
        requests: Mutex<Vec<ShellRequest>>,
        chunks: Vec<ShellOutputChunk>,
    }

    impl ShellTool for ProbeShell {
        fn exec<'a>(
            &'a self,
            request: ShellRequest,
            sink: ShellOutputSink,
            cancellation: tokio_util::sync::CancellationToken,
        ) -> ShellFuture<'a> {
            Box::pin(async move {
                if cancellation.is_cancelled() {
                    return Err(ShellToolError::Cancelled);
                }
                self.requests.lock().expect("lock requests").push(request);
                for chunk in &self.chunks {
                    sink(chunk.clone());
                }
                self.result.clone()
            })
        }
    }

    fn root() -> crate::AbsolutePath {
        crate::AbsolutePath::new(std::env::temp_dir().join("agent-tools-shell-tests"))
            .expect("absolute temp path")
    }

    fn config() -> ShellExecToolConfig {
        ShellExecToolConfig::new(
            Duration::from_secs(120),
            Duration::from_secs(600),
            NonZeroU64::new(1024 * 1024).expect("non-zero"),
        )
        .expect("valid config")
    }

    fn shell(result: Result<ShellOutcome, ShellToolError>) -> Arc<ProbeShell> {
        Arc::new(ProbeShell {
            result,
            requests: Mutex::new(Vec::new()),
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
        })
    }

    fn success() -> ShellOutcome {
        ShellOutcome {
            exit_code: Some(0),
            stdout: "hello\n".to_owned(),
            stderr: "warn\n".to_owned(),
            truncated: false,
            process_mode: ShellProcessMode::Managed,
        }
    }

    fn tool(shell: Arc<dyn ShellTool>) -> ShellExecTool {
        ShellExecTool::new(shell, SessionPathResolver::new(root()), config())
    }

    #[test]
    fn schema_defaults_and_resolution_expose_all_effective_values() {
        let mut registry = ToolRegistry::new();
        registry
            .register(tool(shell(Ok(success()))))
            .expect("register");
        let snapshot = registry.snapshot();
        let definition = &snapshot.definitions()[0];
        assert_eq!(
            definition.input_schema["properties"]["workdir"]["default"],
            root().as_str()
        );
        assert_eq!(
            definition.input_schema["properties"]["timeout_ms"]["default"],
            120_000
        );
        assert_eq!(
            definition.input_schema["properties"]["process_mode"]["default"],
            "managed"
        );

        let batch = Dispatcher::resolve_batch(
            &snapshot,
            &[tool_call("shell", json!({"command": "printf hello"}))],
        );
        let Some(ResolvedBatchItemRef::Valid(invocation)) = batch.get(0) else {
            panic!("shell resolves");
        };
        assert_eq!(invocation.resolved_arguments()["workdir"], root().as_str());
        assert_eq!(invocation.resolved_arguments()["timeout_ms"], 120_000);
        assert_eq!(
            invocation.resolved_arguments()["max_output_bytes"],
            1024 * 1024
        );
        let facts = invocation
            .facts::<ShellAuthorizationFacts>()
            .expect("shell facts");
        assert_eq!(facts.command, "printf hello");
        assert_eq!(facts.workdir, root());
        assert_eq!(facts.timeout, Duration::from_secs(120));
        assert_eq!(facts.process_mode, ShellProcessMode::Managed);
    }

    #[test]
    fn fingerprint_excludes_execution_limits() {
        let mut registry = ToolRegistry::new();
        registry
            .register(tool(shell(Ok(success()))))
            .expect("register");
        let snapshot = registry.snapshot();
        let calls = [
            tool_call(
                "shell",
                json!({"command": "echo hello", "timeout_ms": 1000}),
            ),
            tool_call(
                "shell",
                json!({"command": "echo hello", "timeout_ms": 2000}),
            ),
            tool_call(
                "shell",
                json!({"command": "echo hello", "timeout_ms": 2000, "process_mode": "detached"}),
            ),
        ];
        let batch = Dispatcher::resolve_batch(&snapshot, &calls);
        let (
            Some(ResolvedBatchItemRef::Valid(first)),
            Some(ResolvedBatchItemRef::Valid(second)),
            Some(ResolvedBatchItemRef::Valid(detached)),
        ) = (batch.get(0), batch.get(1), batch.get(2))
        else {
            panic!("shell calls resolve");
        };
        assert_ne!(first.resolved_arguments(), second.resolved_arguments());
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_ne!(second.fingerprint(), detached.fingerprint());
        assert_eq!(
            detached
                .facts::<ShellAuthorizationFacts>()
                .expect("detached facts")
                .process_mode,
            ShellProcessMode::Detached
        );
    }

    #[test]
    fn execution_keeps_channels_and_exact_request() {
        let probe = shell(Ok(success()));
        let collected: Arc<Mutex<Vec<ToolOutputChunk>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_collected = collected.clone();
        let context = ToolContext::new(
            tokio_util::sync::CancellationToken::new(),
            Arc::new(move |chunk| sink_collected.lock().expect("lock output").push(chunk)),
        );
        let resolution = tool(probe.clone())
            .resolve(ShellInput {
                command: "echo hello".to_owned(),
                workdir: Some("subdir".to_owned()),
                timeout_ms: NonZeroU64::new(5_000),
                process_mode: None,
            })
            .expect("resolve");
        let outcome = block_on(tool(probe.clone()).execute(resolution.into_input(), context))
            .expect("execute");
        assert_eq!(outcome, success());
        let requests = probe.requests.lock().expect("lock requests");
        assert_eq!(
            requests[0].workdir.as_path(),
            root().as_path().join("subdir")
        );
        assert_eq!(requests[0].timeout, Duration::from_secs(5));
        assert_eq!(collected.lock().expect("lock output").len(), 2);
    }

    #[test]
    fn invalid_values_and_timeout_error_are_structured() {
        let standard_tool = tool(shell(Ok(success())));
        assert!(
            standard_tool
                .resolve(ShellInput {
                    command: " ".to_owned(),
                    workdir: None,
                    timeout_ms: None,
                    process_mode: None,
                })
                .is_err()
        );
        assert!(
            standard_tool
                .resolve(ShellInput {
                    command: "echo".to_owned(),
                    workdir: None,
                    timeout_ms: NonZeroU64::new(600_001),
                    process_mode: None,
                })
                .is_err()
        );

        let timeout = shell(Err(ShellToolError::TimedOut {
            stdout: "partial out".to_owned(),
            stderr: "partial err".to_owned(),
            truncated: true,
        }));
        let resolution = tool(timeout.clone())
            .resolve(ShellInput {
                command: "sleep 10".to_owned(),
                workdir: None,
                timeout_ms: None,
                process_mode: None,
            })
            .expect("resolve");
        let error =
            block_on(tool(timeout).execute(resolution.into_input(), ToolContext::default()))
                .expect_err("timeout");
        let ToolError::Execution { details, .. } = error else {
            panic!("execution error");
        };
        assert_eq!(details.expect("details")["type"], "timeout");
    }

    #[test]
    fn configuration_rejects_incoherent_limits() {
        assert!(
            ShellExecToolConfig::new(
                Duration::ZERO,
                Duration::from_secs(1),
                NonZeroU64::new(1).expect("non-zero"),
            )
            .is_err()
        );
        assert!(
            ShellExecToolConfig::new(
                Duration::from_nanos(1),
                Duration::from_secs(1),
                NonZeroU64::new(1).expect("non-zero"),
            )
            .is_err()
        );
        assert!(
            ShellExecToolConfig::new(
                Duration::from_secs(2),
                Duration::from_secs(1),
                NonZeroU64::new(1).expect("non-zero"),
            )
            .is_err()
        );
    }
}
