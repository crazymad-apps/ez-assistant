//! Shell stdout/stderr 排空、限流、流式通知与 UTF-8 增量解码。

use agent_tools::{
    ShellOutputChannel, ShellOutputChunk, ShellOutputSink, ShellToolError, utf8_prefix,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

pub(super) const OUTPUT_CHANNEL_CAPACITY: usize = 32;

pub(super) struct RawOutput {
    pub(super) channel: ShellOutputChannel,
    pub(super) bytes: Vec<u8>,
}

pub(super) async fn drain_output(
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

pub(super) enum OutputCompletion {
    Complete(CollectedOutput),
    TimedOut(CollectedOutput),
    Cancelled,
}

pub(super) async fn settle_output_until(
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

pub(super) async fn abort_and_settle_output(
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

pub(super) async fn collect_output(
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

pub(super) struct OutputCollector {
    maximum_bytes: usize,
    retained_bytes: usize,
    pub(super) truncated: bool,
    pub(super) stdout: String,
    pub(super) stderr: String,
    stdout_decoder: IncrementalUtf8Decoder,
    stderr_decoder: IncrementalUtf8Decoder,
    sink: ShellOutputSink,
}

impl OutputCollector {
    pub(super) fn new(maximum_bytes: usize, sink: ShellOutputSink) -> Self {
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

    pub(super) fn push(&mut self, output: RawOutput) {
        if self.truncated {
            return;
        }
        let text = match output.channel {
            ShellOutputChannel::Stdout => self.stdout_decoder.push(&output.bytes),
            ShellOutputChannel::Stderr => self.stderr_decoder.push(&output.bytes),
        };
        self.append_limited(output.channel, text);
    }

    pub(super) fn finish(mut self) -> CollectedOutput {
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

pub(super) struct CollectedOutput {
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) truncated: bool,
}

/// 保留未完整的 UTF-8 尾部，避免将横跨两次读取的合法字符误判为乱码。
#[derive(Default)]
pub(super) struct IncrementalUtf8Decoder {
    pending: Vec<u8>,
}

impl IncrementalUtf8Decoder {
    pub(super) fn push(&mut self, bytes: &[u8]) -> String {
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

    pub(super) fn finish(&mut self) -> String {
        let output = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        output
    }
}
