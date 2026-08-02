//! 真实本地 Shell 能力实现。

use std::{
    ffi::OsString,
    process::{ExitStatus, Stdio},
};

use agent_tools::{
    ShellFuture, ShellOutcome, ShellOutputChannel, ShellOutputChunk, ShellOutputSink,
    ShellProcessMode, ShellRequest, ShellTool, ShellToolError, utf8_prefix,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

use crate::{
    environment::EnvironmentPolicy,
    process::{self, ManagedChild},
};

const OUTPUT_CHANNEL_CAPACITY: usize = 32;
const PARENT_STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// 平台 Shell 启动器。
///
/// `program` 和 `fixed_args` 是 Adapter 配置，不是模型输入。执行时会将
/// 模型给出的完整 command 原样追加为最后一个参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellLauncher {
    pub program: OsString,
    pub fixed_args: Vec<OsString>,
}

#[cfg(unix)]
impl Default for ShellLauncher {
    fn default() -> Self {
        Self {
            program: OsString::from("/bin/sh"),
            fixed_args: vec![OsString::from("-c")],
        }
    }
}

#[cfg(windows)]
impl Default for ShellLauncher {
    fn default() -> Self {
        let program = std::env::var_os("COMSPEC")
            .filter(|program| !program.is_empty())
            .unwrap_or_else(|| OsString::from("cmd.exe"));
        Self {
            program,
            fixed_args: vec![OsString::from("/C")],
        }
    }
}

/// 本地 Shell Adapter 的构造配置。
///
/// 环境策略必须由宿主显式传入，避免 Adapter 暗中假定具体 Provider
/// credential 名称。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalShellConfig {
    pub launcher: ShellLauncher,
    pub environment: EnvironmentPolicy,
}

impl LocalShellConfig {
    /// 使用平台默认 launcher 和显式环境策略构造配置。
    pub fn new(environment: EnvironmentPolicy) -> Self {
        Self {
            launcher: ShellLauncher::default(),
            environment,
        }
    }
}

/// 使用当前用户权限启动平台 Shell 的本地 Adapter。
///
/// 它不是 OS 沙箱：workdir 只决定子进程的初始目录，Shell 仍然可以
/// 使用当前用户权限访问其他路径。
#[derive(Debug)]
pub struct LocalShell {
    config: LocalShellConfig,
}

impl LocalShell {
    pub fn new(config: LocalShellConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LocalShellConfig {
        &self.config
    }

    async fn run(
        &self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> Result<ShellOutcome, ShellToolError> {
        if request.command.trim().is_empty() {
            return Err(ShellToolError::InvalidInput {
                message: "command must not be empty".to_owned(),
            });
        }
        if cancellation.is_cancelled() {
            return Err(ShellToolError::Cancelled);
        }

        let mut command = Command::new(&self.config.launcher.program);
        command
            .args(&self.config.launcher.fixed_args)
            .arg(&request.command)
            .current_dir(request.workdir.as_path())
            .env_clear()
            .envs(self.config.environment.resolve_current())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let spawn = match request.process_mode {
            ShellProcessMode::Managed => process::spawn,
            ShellProcessMode::Detached => process::spawn_detachable,
        };
        let mut child = spawn(command).map_err(|error| ShellToolError::Io {
            message: format!("start shell failed: {error}"),
        })?;
        let stdout = match child.stdout().take() {
            Some(stdout) => stdout,
            None => {
                cleanup_after_setup_failure(&mut child).await?;
                return Err(ShellToolError::Io {
                    message: "shell stdout pipe was not created".to_owned(),
                });
            }
        };
        let stderr = match child.stderr().take() {
            Some(stderr) => stderr,
            None => {
                cleanup_after_setup_failure(&mut child).await?;
                return Err(ShellToolError::Io {
                    message: "shell stderr pipe was not created".to_owned(),
                });
            }
        };

        let (sender, receiver) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        let stdout_task = tokio::spawn(drain_output(
            stdout,
            ShellOutputChannel::Stdout,
            sender.clone(),
        ));
        let stderr_task = tokio::spawn(drain_output(
            stderr,
            ShellOutputChannel::Stderr,
            sender.clone(),
        ));
        drop(sender);
        let output_task = tokio::spawn(collect_output(
            receiver,
            request.max_output_bytes.get(),
            sink,
        ));

        let deadline = Instant::now() + request.timeout;
        let process_completion = wait_for_parent(&mut child, deadline, &cancellation).await?;
        match process_completion {
            ProcessCompletion::Exited(status) => {
                if request.process_mode == ShellProcessMode::Managed {
                    process::terminate_and_wait(&mut child)
                        .await
                        .map_err(|error| ShellToolError::Io {
                            message: format!(
                                "clean up managed shell process tree after parent exit failed: {error}"
                            ),
                        })?;
                }
                let output_completion = settle_output_until(
                    stdout_task,
                    stderr_task,
                    output_task,
                    deadline,
                    &cancellation,
                )
                .await?;
                match output_completion {
                    OutputCompletion::Complete(output) => {
                        // Detached 使用不带 kill-on-drop 的包装；只在父进程退出且两个输出
                        // 管道 EOF 后 drop，明确把仍存活的后代交给操作系统继续管理。
                        drop(child);
                        Ok(ShellOutcome {
                            exit_code: status.code(),
                            stdout: output.stdout,
                            stderr: output.stderr,
                            truncated: output.truncated,
                            process_mode: request.process_mode,
                        })
                    }
                    OutputCompletion::TimedOut(output) => {
                        process::terminate_and_wait(&mut child).await.map_err(|error| {
                            ShellToolError::Io {
                                message: format!(
                                    "terminate shell process tree during output settlement failed: {error}"
                                ),
                            }
                        })?;
                        Err(ShellToolError::TimedOut {
                            stdout: output.stdout,
                            stderr: output.stderr,
                            truncated: output.truncated,
                        })
                    }
                    OutputCompletion::Cancelled => {
                        process::terminate_and_wait(&mut child).await.map_err(|error| {
                            ShellToolError::Io {
                                message: format!(
                                    "terminate cancelled shell process tree during output settlement failed: {error}"
                                ),
                            }
                        })?;
                        Err(ShellToolError::Cancelled)
                    }
                }
            }
            ProcessCompletion::TimedOut => {
                process::terminate_and_wait(&mut child)
                    .await
                    .map_err(|error| ShellToolError::Io {
                        message: format!("terminate timed out shell process tree failed: {error}"),
                    })?;
                let output = abort_and_settle_output(stdout_task, stderr_task, output_task).await?;
                Err(ShellToolError::TimedOut {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    truncated: output.truncated,
                })
            }
            ProcessCompletion::Cancelled => {
                process::terminate_and_wait(&mut child)
                    .await
                    .map_err(|error| ShellToolError::Io {
                        message: format!("terminate cancelled shell process tree failed: {error}"),
                    })?;
                abort_and_settle_output(stdout_task, stderr_task, output_task).await?;
                Err(ShellToolError::Cancelled)
            }
        }
    }
}

impl ShellTool for LocalShell {
    fn exec<'a>(
        &'a self,
        request: ShellRequest,
        sink: ShellOutputSink,
        cancellation: CancellationToken,
    ) -> ShellFuture<'a> {
        Box::pin(self.run(request, sink, cancellation))
    }
}

enum ProcessCompletion {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

async fn wait_for_parent(
    child: &mut ManagedChild,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<ProcessCompletion, ShellToolError> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| ShellToolError::Io {
            message: format!("check shell parent status failed: {error}"),
        })? {
            return Ok(ProcessCompletion::Exited(status));
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(ProcessCompletion::Cancelled),
            _ = sleep_until(deadline) => return Ok(ProcessCompletion::TimedOut),
            _ = tokio::time::sleep(PARENT_STATUS_POLL_INTERVAL) => {}
        }
    }
}

async fn cleanup_after_setup_failure(child: &mut ManagedChild) -> Result<(), ShellToolError> {
    process::terminate_and_wait(child)
        .await
        .map(|_| ())
        .map_err(|error| ShellToolError::Io {
            message: format!("clean up shell after pipe setup failure failed: {error}"),
        })
}

struct RawOutput {
    channel: ShellOutputChannel,
    bytes: Vec<u8>,
}

async fn drain_output(
    mut reader: impl AsyncRead + Unpin,
    channel: ShellOutputChannel,
    sender: mpsc::Sender<RawOutput>,
) -> std::io::Result<()> {
    let mut buffer = vec![0_u8; 8 * 1024];
    let mut receiver_is_open = true;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        if receiver_is_open
            && sender
                .send(RawOutput {
                    channel,
                    bytes: buffer[..read].to_vec(),
                })
                .await
                .is_err()
        {
            // 汇流任务异常结束时仍继续排空管道，避免子进程受反压阻塞。
            receiver_is_open = false;
        }
    }
}

async fn settle_output(
    stdout_task: JoinHandle<std::io::Result<()>>,
    stderr_task: JoinHandle<std::io::Result<()>>,
    output_task: JoinHandle<CollectedOutput>,
) -> Result<CollectedOutput, ShellToolError> {
    let stdout_result = join_reader("stdout", stdout_task).await;
    let stderr_result = join_reader("stderr", stderr_task).await;
    let output_result = output_task.await.map_err(|error| ShellToolError::Io {
        message: format!("shell output collector task failed: {error}"),
    });

    stdout_result?;
    stderr_result?;
    output_result
}

enum OutputCompletion {
    Complete(CollectedOutput),
    TimedOut(CollectedOutput),
    Cancelled,
}

async fn settle_output_until(
    stdout_task: JoinHandle<std::io::Result<()>>,
    stderr_task: JoinHandle<std::io::Result<()>>,
    output_task: JoinHandle<CollectedOutput>,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<OutputCompletion, ShellToolError> {
    let stdout_abort = stdout_task.abort_handle();
    let stderr_abort = stderr_task.abort_handle();
    let settle = settle_output(stdout_task, stderr_task, output_task);
    tokio::pin!(settle);
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            stdout_abort.abort();
            stderr_abort.abort();
            settle.await?;
            Ok(OutputCompletion::Cancelled)
        }
        _ = sleep_until(deadline) => {
            stdout_abort.abort();
            stderr_abort.abort();
            Ok(OutputCompletion::TimedOut(settle.await?))
        }
        output = &mut settle => output.map(OutputCompletion::Complete),
    }
}

async fn abort_and_settle_output(
    stdout_task: JoinHandle<std::io::Result<()>>,
    stderr_task: JoinHandle<std::io::Result<()>>,
    output_task: JoinHandle<CollectedOutput>,
) -> Result<CollectedOutput, ShellToolError> {
    stdout_task.abort();
    stderr_task.abort();
    settle_output(stdout_task, stderr_task, output_task).await
}

async fn join_reader(
    name: &str,
    task: JoinHandle<std::io::Result<()>>,
) -> Result<(), ShellToolError> {
    match task.await {
        Ok(result) => result.map_err(|error| ShellToolError::Io {
            message: format!("read shell {name} failed: {error}"),
        }),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(ShellToolError::Io {
            message: format!("shell {name} reader task failed: {error}"),
        }),
    }
}

async fn collect_output(
    mut receiver: mpsc::Receiver<RawOutput>,
    maximum_bytes: u64,
    sink: ShellOutputSink,
) -> CollectedOutput {
    let maximum_bytes = usize::try_from(maximum_bytes).unwrap_or(usize::MAX);
    let mut collector = OutputCollector::new(maximum_bytes, sink);
    while let Some(output) = receiver.recv().await {
        collector.push(output);
    }
    collector.finish()
}

struct OutputCollector {
    maximum_bytes: usize,
    retained_bytes: usize,
    truncated: bool,
    stdout: String,
    stderr: String,
    stdout_decoder: IncrementalUtf8Decoder,
    stderr_decoder: IncrementalUtf8Decoder,
    sink: ShellOutputSink,
}

impl OutputCollector {
    fn new(maximum_bytes: usize, sink: ShellOutputSink) -> Self {
        Self {
            maximum_bytes,
            retained_bytes: 0,
            truncated: false,
            stdout: String::new(),
            stderr: String::new(),
            stdout_decoder: IncrementalUtf8Decoder::default(),
            stderr_decoder: IncrementalUtf8Decoder::default(),
            sink,
        }
    }

    fn push(&mut self, output: RawOutput) {
        if self.truncated {
            return;
        }
        let text = match output.channel {
            ShellOutputChannel::Stdout => self.stdout_decoder.push(&output.bytes),
            ShellOutputChannel::Stderr => self.stderr_decoder.push(&output.bytes),
        };
        self.append_limited(output.channel, text);
    }

    fn finish(mut self) -> CollectedOutput {
        if !self.truncated {
            let stdout_tail = self.stdout_decoder.finish();
            self.append_limited(ShellOutputChannel::Stdout, stdout_tail);
        }
        if !self.truncated {
            let stderr_tail = self.stderr_decoder.finish();
            self.append_limited(ShellOutputChannel::Stderr, stderr_tail);
        }
        CollectedOutput {
            stdout: self.stdout,
            stderr: self.stderr,
            truncated: self.truncated,
        }
    }

    fn append_limited(&mut self, channel: ShellOutputChannel, text: String) {
        let remaining = self.maximum_bytes.saturating_sub(self.retained_bytes);
        let retained = utf8_prefix(&text, remaining);
        if retained.len() < text.len() {
            self.truncated = true;
        }
        if retained.is_empty() {
            return;
        }
        self.retained_bytes += retained.len();
        self.append(channel, retained.to_owned());
    }

    fn append(&mut self, channel: ShellOutputChannel, text: String) {
        if text.is_empty() {
            return;
        }
        match channel {
            ShellOutputChannel::Stdout => self.stdout.push_str(&text),
            ShellOutputChannel::Stderr => self.stderr.push_str(&text),
        }
        (self.sink)(ShellOutputChunk {
            channel,
            data: text,
        });
    }
}

struct CollectedOutput {
    stdout: String,
    stderr: String,
    truncated: bool,
}

/// 保留未完整的 UTF-8 尾部，避免将横跨两次读取的合法字符误判为乱码。
#[derive(Default)]
struct IncrementalUtf8Decoder {
    pending: Vec<u8>,
}

impl IncrementalUtf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    output.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        // SAFETY 不需要 unsafe：valid_up_to 由 from_utf8 验证结果给出。
                        if let Ok(valid) = std::str::from_utf8(&self.pending[..valid_up_to]) {
                            output.push_str(valid);
                        }
                        self.pending.drain(..valid_up_to);
                    }
                    let Some(error_len) = error.error_len() else {
                        break;
                    };
                    output.push('\u{FFFD}');
                    self.pending.drain(..error_len);
                }
            }
        }
        output
    }

    fn finish(&mut self) -> String {
        let output = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        output
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        num::NonZeroU64,
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[cfg(unix)]
    use std::{
        collections::{BTreeMap, BTreeSet},
        os::unix::fs::PermissionsExt,
    };

    use agent_tools::AbsolutePath;
    use tempfile::TempDir;

    use super::*;

    fn shell(environment: EnvironmentPolicy) -> LocalShell {
        LocalShell::new(LocalShellConfig::new(environment))
    }

    fn request(directory: &TempDir, command: &str) -> ShellRequest {
        ShellRequest {
            command: command.to_owned(),
            workdir: AbsolutePath::new(directory.path().to_path_buf()).expect("absolute temp path"),
            timeout: Duration::from_secs(5),
            max_output_bytes: NonZeroU64::new(1024).expect("non-zero output limit"),
            process_mode: ShellProcessMode::Managed,
        }
    }

    fn recording_sink() -> (ShellOutputSink, Arc<Mutex<Vec<ShellOutputChunk>>>) {
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let target = chunks.clone();
        let sink: ShellOutputSink = Arc::new(move |chunk| {
            target.lock().expect("lock output chunks").push(chunk);
        });
        (sink, chunks)
    }

    #[cfg(unix)]
    async fn process_exists(pid: u32) -> bool {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -0 {pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .expect("query process")
            .success()
    }

    #[cfg(unix)]
    async fn stop_process(pid: u32) {
        let _ = Command::new("/bin/kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while process_exists(pid).await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("temporary detached process must stop");
    }

    #[test]
    fn decoder_preserves_cross_chunk_utf8_and_replaces_invalid_bytes() {
        let mut decoder = IncrementalUtf8Decoder::default();
        assert_eq!(decoder.push(&[0xE4]), "");
        assert_eq!(decoder.push(&[0xB8, 0xAD, b'a', 0xFF]), "中a�");
        assert_eq!(decoder.finish(), "");

        let mut incomplete = IncrementalUtf8Decoder::default();
        assert_eq!(incomplete.push(&[0xE4, 0xB8]), "");
        assert_eq!(incomplete.finish(), "\u{FFFD}");
    }

    #[test]
    fn output_limit_never_splits_utf8_or_exceeds_result_bytes() {
        let (sink, chunks) = recording_sink();
        let mut collector = OutputCollector::new(2, sink);
        collector.push(RawOutput {
            channel: ShellOutputChannel::Stdout,
            bytes: "中".as_bytes().to_vec(),
        });
        let output = collector.finish();

        assert_eq!(output.stdout, "");
        assert!(output.truncated);
        assert!(chunks.lock().expect("lock chunks").is_empty());

        let (sink, _) = recording_sink();
        let mut collector = OutputCollector::new(3, sink);
        collector.push(RawOutput {
            channel: ShellOutputChannel::Stderr,
            bytes: vec![0xFF],
        });
        let output = collector.finish();
        assert_eq!(output.stderr, "\u{FFFD}");
        assert_eq!(output.stderr.len(), 3);
        assert!(!output.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_launcher_receives_fixed_args_then_complete_command() {
        let directory = TempDir::new().expect("create temp directory");
        let launcher_path = directory.path().join("launcher.sh");
        std::fs::write(
            &launcher_path,
            "#!/bin/sh\nprintf '%s\\n%s' \"$1\" \"$2\"\n",
        )
        .expect("write launcher script");
        let mut permissions = std::fs::metadata(&launcher_path)
            .expect("read launcher metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&launcher_path, permissions).expect("make launcher executable");
        let shell = LocalShell::new(LocalShellConfig {
            launcher: ShellLauncher {
                program: launcher_path.into_os_string(),
                fixed_args: vec![OsString::from("fixed")],
            },
            environment: EnvironmentPolicy::default(),
        });
        let (sink, _) = recording_sink();
        let command = "printf 'not executed' | cat";

        let outcome = shell
            .exec(request(&directory, command), sink, CancellationToken::new())
            .await
            .expect("execute custom launcher");

        assert_eq!(outcome.stdout, format!("fixed\n{command}"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn executes_shell_syntax_and_keeps_nonzero_exit_as_outcome() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, chunks) = recording_sink();
        let outcome = shell
            .exec(
                request(
                    &directory,
                    "printf 'hello' | tr 'a-z' 'A-Z'; printf 'problem' >&2; exit 7",
                ),
                sink,
                CancellationToken::new(),
            )
            .await
            .expect("execute shell command");

        assert_eq!(outcome.exit_code, Some(7));
        assert_eq!(outcome.stdout, "HELLO");
        assert_eq!(outcome.stderr, "problem");
        assert!(!outcome.truncated);
        let chunks = chunks.lock().expect("lock chunks");
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.channel == ShellOutputChannel::Stdout)
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.channel == ShellOutputChannel::Stderr)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn applies_workdir_and_filtered_environment() {
        let directory = TempDir::new().expect("create temp directory");
        let environment = EnvironmentPolicy {
            allow_exact: BTreeSet::new(),
            deny_exact: BTreeSet::new(),
            deny_suffixes: vec![OsString::from("_SECRET")],
            overrides: BTreeMap::from([
                (
                    OsString::from("LOCAL_SHELL_VISIBLE"),
                    Some(OsString::from("visible")),
                ),
                (
                    OsString::from("LOCAL_SHELL_SECRET"),
                    Some(OsString::from("removed-by-later-policy-test")),
                ),
            ]),
        };
        // override 最后生效，因此单独删除需要显式 None。
        let environment = EnvironmentPolicy {
            overrides: BTreeMap::from([
                (
                    OsString::from("LOCAL_SHELL_VISIBLE"),
                    Some(OsString::from("visible")),
                ),
                (OsString::from("LOCAL_SHELL_SECRET"), None),
            ]),
            ..environment
        };
        let shell = shell(environment);
        let (sink, _) = recording_sink();
        let outcome = shell
            .exec(
                request(
                    &directory,
                    "pwd; printf '%s|%s' \"$LOCAL_SHELL_VISIBLE\" \"${LOCAL_SHELL_SECRET-unset}\"",
                ),
                sink,
                CancellationToken::new(),
            )
            .await
            .expect("execute environment command");

        // macOS 上 `/var` 等路径可能是系统链接，`pwd` 展示子进程看到的实际目录。
        let actual_workdir = std::fs::canonicalize(directory.path()).expect("resolve temp workdir");
        let expected_prefix = format!("{}\n", actual_workdir.display());
        assert!(outcome.stdout.starts_with(&expected_prefix));
        assert!(outcome.stdout.ends_with("visible|unset"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn closes_stdin_and_limits_combined_output_without_deadlock() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, chunks) = recording_sink();
        let mut request = request(
            &directory,
            "if read value; then printf 'open'; else printf 'closed'; fi; \
             i=0; while [ \"$i\" -lt 20000 ]; do printf 'abcdefghij' >&2; i=$((i + 1)); done",
        );
        request.max_output_bytes = NonZeroU64::new(7).expect("non-zero output limit");
        let outcome = shell
            .exec(request, sink, CancellationToken::new())
            .await
            .expect("execute limited output command");

        assert_eq!(outcome.stdout.len() + outcome.stderr.len(), 7);
        assert!(outcome.truncated);
        let streamed = chunks
            .lock()
            .expect("lock chunks")
            .iter()
            .map(|chunk| chunk.data.len())
            .sum::<usize>();
        assert_eq!(streamed, 7);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_returns_partial_output_after_cleanup() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, _) = recording_sink();
        let mut request = request(&directory, "printf 'before-timeout'; sleep 30");
        request.timeout = Duration::from_millis(100);
        let error = shell
            .exec(request, sink, CancellationToken::new())
            .await
            .expect_err("command must time out");

        assert_eq!(
            error,
            ShellToolError::TimedOut {
                stdout: "before-timeout".to_owned(),
                stderr: String::new(),
                truncated: false,
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_waits_for_cleanup() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let cancellation = CancellationToken::new();
        let cancel_from_sink = cancellation.clone();
        let sink: ShellOutputSink = Arc::new(move |chunk| {
            if chunk.data.contains("ready") {
                cancel_from_sink.cancel();
            }
        });
        let error = shell
            .exec(
                request(&directory, "printf 'ready'; sleep 30"),
                sink,
                cancellation,
            )
            .await
            .expect_err("command must be cancelled");

        assert_eq!(error, ShellToolError::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_parent_exit_still_terminates_background_children() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, _) = recording_sink();
        let outcome = shell
            .exec(
                request(
                    &directory,
                    "sleep 30 >/dev/null 2>&1 & child=$!; printf '%s' \"$child\"",
                ),
                sink,
                CancellationToken::new(),
            )
            .await
            .expect("managed command completes");
        let pid = outcome.stdout.parse::<u32>().expect("parse child pid");
        assert!(!process_exists(pid).await, "managed child {pid} survived");
        assert_eq!(outcome.process_mode, ShellProcessMode::Managed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_redirected_background_child_survives_explicit_handoff() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, _) = recording_sink();
        let mut detached = request(
            &directory,
            "sleep 30 </dev/null >/dev/null 2>&1 & child=$!; printf '%s' \"$child\"",
        );
        detached.process_mode = ShellProcessMode::Detached;
        let outcome = shell
            .exec(detached, sink, CancellationToken::new())
            .await
            .expect("detached command hands off");
        let pid = outcome.stdout.parse::<u32>().expect("parse child pid");
        let survived = process_exists(pid).await;
        stop_process(pid).await;
        assert!(survived, "detached child did not survive handoff");
        assert_eq!(outcome.process_mode, ShellProcessMode::Detached);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires python3 for a detached loopback service smoke test"]
    async fn detached_loopback_service_is_reachable_and_explicitly_stopped() {
        use std::io::{Read, Write};

        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, _) = recording_sink();
        let mut detached = request(
            &directory,
            "python3 -c 'import http.server; s=http.server.ThreadingHTTPServer((\"127.0.0.1\", 0), http.server.SimpleHTTPRequestHandler); open(\"service.port\", \"w\").write(str(s.server_port)); s.serve_forever()' </dev/null >service.log 2>&1 & child=$!; printf '%s' \"$child\"",
        );
        detached.process_mode = ShellProcessMode::Detached;
        let outcome = shell
            .exec(detached, sink, CancellationToken::new())
            .await
            .expect("detached service hands off");
        let pid = outcome.stdout.parse::<u32>().expect("parse service pid");
        let port_file = directory.path().join("service.port");
        let validation = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(port) = tokio::fs::read_to_string(&port_file).await {
                    let address = format!("127.0.0.1:{}", port.trim());
                    if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
                        &address.parse().expect("parse loopback address"),
                        Duration::from_millis(100),
                    ) {
                        stream
                            .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
                            .expect("write HTTP probe");
                        let mut response = String::new();
                        stream
                            .read_to_string(&mut response)
                            .expect("read HTTP probe");
                        break response;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        stop_process(pid).await;
        let response = validation.expect("detached service becomes reachable");
        assert!(response.starts_with("HTTP/1.0 200"));
        assert!(!process_exists(pid).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_inherited_output_is_cleaned_up_on_settlement_timeout() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let (sink, _) = recording_sink();
        let mut detached = request(&directory, "sleep 30 & child=$!; printf '%s' \"$child\"");
        detached.process_mode = ShellProcessMode::Detached;
        detached.timeout = Duration::from_millis(150);
        let error = shell
            .exec(detached, sink, CancellationToken::new())
            .await
            .expect_err("inherited pipes keep settlement open until timeout");
        let ShellToolError::TimedOut { stdout, .. } = error else {
            panic!("expected timeout");
        };
        let pid = stdout.parse::<u32>().expect("parse timed out child pid");
        assert!(!process_exists(pid).await, "timed out child {pid} survived");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_output_settlement_responds_to_cancellation() {
        let directory = TempDir::new().expect("create temp directory");
        let pid_file = directory.path().join("child.pid");
        let shell = shell(EnvironmentPolicy::default());
        let cancellation = CancellationToken::new();
        let cancellation_trigger = cancellation.clone();
        let marker = pid_file.clone();
        let trigger = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !tokio::fs::try_exists(&marker)
                    .await
                    .expect("check pid marker")
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("parent writes pid marker");
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancellation_trigger.cancel();
        });
        let (sink, _) = recording_sink();
        let mut detached = request(
            &directory,
            &format!(
                "sleep 30 & child=$!; printf '%s' \"$child\" > {}",
                pid_file.display()
            ),
        );
        detached.process_mode = ShellProcessMode::Detached;
        let error = shell
            .exec(detached, sink, cancellation)
            .await
            .expect_err("settlement is cancelled");
        trigger.await.expect("cancellation trigger");
        assert_eq!(error, ShellToolError::Cancelled);
        let pid = tokio::fs::read_to_string(&pid_file)
            .await
            .expect("read child pid")
            .parse::<u32>()
            .expect("parse child pid");
        assert!(!process_exists(pid).await, "cancelled child {pid} survived");
    }

    #[tokio::test]
    async fn pre_cancelled_request_does_not_spawn_launcher() {
        let directory = TempDir::new().expect("create temp directory");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let shell = LocalShell::new(LocalShellConfig {
            launcher: ShellLauncher {
                program: OsString::from("definitely-missing-shell-launcher"),
                fixed_args: Vec::new(),
            },
            environment: EnvironmentPolicy::default(),
        });
        let (sink, _) = recording_sink();

        let error = shell
            .exec(request(&directory, "ignored"), sink, cancellation)
            .await
            .expect_err("pre-cancelled request must not spawn");
        assert_eq!(error, ShellToolError::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "runs a real process-tree cleanup smoke test"]
    async fn cancellation_kills_background_process_tree() {
        let directory = TempDir::new().expect("create temp directory");
        let shell = shell(EnvironmentPolicy::default());
        let cancellation = CancellationToken::new();
        let cancel_from_sink = cancellation.clone();
        let pid_text = Arc::new(Mutex::new(String::new()));
        let pid_target = pid_text.clone();
        let sink: ShellOutputSink = Arc::new(move |chunk| {
            if chunk.channel == ShellOutputChannel::Stdout {
                pid_target
                    .lock()
                    .expect("lock child pid")
                    .push_str(&chunk.data);
                cancel_from_sink.cancel();
            }
        });
        let error = shell
            .exec(
                request(
                    &directory,
                    "sleep 30 & child=$!; printf '%s' \"$child\"; wait",
                ),
                sink,
                cancellation,
            )
            .await
            .expect_err("process tree must be cancelled");
        assert_eq!(error, ShellToolError::Cancelled);

        let pid = pid_text
            .lock()
            .expect("lock child pid")
            .trim()
            .parse::<u32>()
            .expect("parse child pid");
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -0 {pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .expect("query child process");
        assert!(!status.success(), "background child {pid} is still running");
    }
}
