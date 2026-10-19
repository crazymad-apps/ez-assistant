//! 原生 helper IPC：独立 stdin 控制、stdout 帧和 stderr 状态；每次读取最多一帧。
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::{io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdout, Command},
    time::timeout,
};

const FRAME_LIMIT: usize = 4 * 1024 * 1024;
const STATUS_LIMIT: usize = 8192;

#[derive(Debug, thiserror::Error)]
pub(crate) enum MediaError {
    #[error("desktop capture is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("desktop transport failed: {0}")]
    Transport(#[from] rtc::shared::error::Error),
    #[error("desktop authorization ended: {0}")]
    Access(#[from] crate::access::AccessError),
    #[error("desktop connection ended")]
    Ended,
    #[error("desktop direct connection timed out")]
    ConnectTimeout,
    #[error("desktop capture helper is missing; rebuild the platform payload")]
    MissingHelper,
    #[error("desktop capture I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid desktop capture protocol")]
    Protocol,
    #[error("desktop capture unavailable: {0}")]
    Unavailable(String),
    #[error("desktop capture helper did not stop gracefully")]
    StopTimeout,
    #[error("desktop capture produced no frames")]
    NoFrames,
}

pub(super) struct Frame {
    pub(super) data: Vec<u8>,
    pub(super) keyframe: bool,
    pub(super) timestamp_us: u64,
}

/// 单显示源进程 owner；未来访问 owner 必须显式 stop。意外丢弃时 kill_on_drop 与 stdin EOF 双兜底。
pub(super) struct Capture {
    child: Child,
    output: BufReader<ChildStdout>,
    status: BufReader<ChildStderr>,
    status_bytes: Vec<u8>,
    previous_timestamp: Option<u64>,
    pub(super) format: serde_json::Value,
}

fn command(helper: &Path) -> Result<Command, MediaError> {
    if !helper.is_file() {
        return Err(MediaError::MissingHelper);
    }
    let mut command = Command::new(helper);
    command.env_clear();
    // 原生框架所需的 OS 用户/临时目录；不继承模型密钥、代理和企业凭据。
    for key in [
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
        "HOME",
        "TMPDIR",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW；仍沿调用者交互会话，不切用户/桌面。
    Ok(command)
}

impl Capture {
    pub(super) async fn start(helper: &Path, display: &str) -> Result<Self, MediaError> {
        Self::start_sized(helper, display, [1920, 1440, 30]).await
    }

    pub(super) fn take_control(&mut self) -> Result<tokio::process::ChildStdin, MediaError> {
        self.child.stdin.take().ok_or(MediaError::Protocol)
    }

    pub(super) async fn start_sized(
        helper: &Path,
        display: &str,
        size: [u32; 3],
    ) -> Result<Self, MediaError> {
        Self::start_bound(helper, display, size, None).await
    }

    pub(super) async fn start_bound(
        helper: &Path,
        display: &str,
        size: [u32; 3],
        binding: Option<&str>,
    ) -> Result<Self, MediaError> {
        // 显示标识是 OS 返回的短字符串；参数数组不经 shell。Windows 标识包含设备路径。
        if display.is_empty() || display.len() > 256 || display.chars().any(char::is_control) {
            return Err(MediaError::Protocol);
        }
        let mut command = command(helper)?;
        command.args([
            "--display",
            display,
            &size[0].to_string(),
            &size[1].to_string(),
            &size[2].to_string(),
        ]);
        if let Some(binding) = binding {
            command.arg(binding);
        }
        let mut child = command.spawn()?;
        let output = BufReader::new(child.stdout.take().ok_or(MediaError::Protocol)?);
        let status = BufReader::new(child.stderr.take().ok_or(MediaError::Protocol)?);
        let mut capture = Self {
            child,
            output,
            status,
            status_bytes: Vec::new(),
            previous_timestamp: None,
            format: serde_json::Value::Null,
        };
        let configured = timeout(
            Duration::from_secs(10),
            read_status(&mut capture.status, &mut capture.status_bytes),
        )
        .await
        .map_err(|_| MediaError::NoFrames)
        .and_then(|value| value)
        .and_then(|value| {
            check_status(value.clone())?;
            if value["codec"] != "h264"
                || !valid_dimensions(value["width"].as_u64(), value["height"].as_u64())
                || !value["fps"].as_u64().is_some_and(|v| (1..=30).contains(&v))
                || !value["hardware"].is_boolean()
            {
                return Err(MediaError::Protocol);
            }
            Ok(value)
        });
        match configured {
            Ok(format) => {
                capture.format = format;
                Ok(capture)
            }
            Err(error) => {
                let _ = capture.stop().await;
                Err(error)
            }
        }
    }

    /// 单消费端读取；调用方取消本 future 后只能 stop，不能继续读取截断的帧。
    /// 状态轮询不会丢弃正在读取的帧 future，避免 read_exact 的取消不安全问题。
    pub(super) async fn next_frame(&mut self) -> Result<Frame, MediaError> {
        let frame = read_frame(&mut self.output);
        tokio::pin!(frame);
        let received = loop {
            tokio::select! {
                result = &mut frame => break result?,
                result = read_status(&mut self.status, &mut self.status_bytes) => { check_status(result?)?; }
            }
        };
        if self
            .previous_timestamp
            .is_some_and(|previous| received.timestamp_us <= previous)
        {
            return Err(MediaError::Protocol);
        }
        if self.previous_timestamp.is_none() && !received.keyframe {
            return Err(MediaError::Protocol);
        }
        self.previous_timestamp = Some(received.timestamp_us);
        Ok(received)
    }

    pub(super) async fn stop(mut self) -> Result<(), MediaError> {
        // 关闭独立控制管道比排在视频队列中的 stop 消息更可靠，helper 把 EOF 视为撤销。
        if let Some(mut input) = self.child.stdin.take() {
            let _ = timeout(Duration::from_millis(100), input.write_all(b"stop\n")).await;
            drop(input);
        }
        match timeout(Duration::from_secs(3), self.child.wait()).await {
            Ok(result) => {
                if result?.success() {
                    Ok(())
                } else {
                    Err(MediaError::Unavailable("helper_failed".into()))
                }
            }
            Err(_) => {
                self.child.kill().await?;
                self.child.wait().await?;
                Err(MediaError::StopTimeout)
            }
        }
    }
}

pub(super) async fn displays(helper: &Path) -> Result<serde_json::Value, MediaError> {
    let mut child = command(helper)?.arg("--list").spawn()?;
    let output = child.stdout.take().ok_or(MediaError::Protocol)?;
    let status = child.stderr.take().ok_or(MediaError::Protocol)?;
    let result = timeout(Duration::from_secs(10), async {
        // 枚举是一次性短响应；两个管道并行读到 EOF，避免退出时 stderr EOF 抢先遮盖 stdout。
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut output = output.take((STATUS_LIMIT + 1) as u64);
        let mut status = status.take((STATUS_LIMIT + 1) as u64);
        let (a, b) = tokio::join!(output.read_to_end(&mut out), status.read_to_end(&mut err));
        a?;
        b?;
        if out.len() > STATUS_LIMIT || err.len() > STATUS_LIMIT {
            return Err(MediaError::Protocol);
        }
        for line in err
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            check_status(serde_json::from_slice(line).map_err(|_| MediaError::Protocol)?)?;
        }
        let value: serde_json::Value =
            serde_json::from_slice(&out).map_err(|_| MediaError::Protocol)?;
        if !value.is_array() {
            return Err(MediaError::Protocol);
        }
        Ok(value)
    })
    .await;
    drop(child.stdin.take());
    // 读取失败同样等待并回收，不把进程残留留给下一次诊断。
    let exit = match timeout(Duration::from_secs(3), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            child.kill().await?;
            child.wait().await?
        }
    };
    let value = result.map_err(|_| MediaError::NoFrames)??;
    if !exit.success() {
        return Err(MediaError::Unavailable("helper_failed".into()));
    }
    Ok(value)
}

async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Frame, MediaError> {
    let size = reader.read_u32().await? as usize;
    if size == 0 || size > FRAME_LIMIT {
        return Err(MediaError::Protocol);
    }
    let timestamp_us = reader.read_u64().await?;
    let key = reader.read_u8().await?;
    if key > 1 {
        return Err(MediaError::Protocol);
    }
    let mut data = vec![0; size];
    reader.read_exact(&mut data).await?;
    if !data.starts_with(&[0, 0, 0, 1]) && !data.starts_with(&[0, 0, 1]) {
        return Err(MediaError::Protocol);
    }
    Ok(Frame {
        data,
        timestamp_us,
        keyframe: key == 1,
    })
}

async fn read_line(
    reader: &mut (impl AsyncRead + Unpin),
    data: &mut Vec<u8>,
    limit: usize,
) -> Result<Vec<u8>, MediaError> {
    loop {
        let byte = reader.read_u8().await?;
        if byte == b'\n' {
            return Ok(std::mem::take(data));
        }
        if data.len() == limit {
            return Err(MediaError::Protocol);
        }
        data.push(byte);
    }
}

async fn read_status(
    reader: &mut (impl AsyncRead + Unpin),
    partial: &mut Vec<u8>,
) -> Result<serde_json::Value, MediaError> {
    serde_json::from_slice(&read_line(reader, partial, STATUS_LIMIT).await?)
        .map_err(|_| MediaError::Protocol)
}

fn check_status(status: serde_json::Value) -> Result<(), MediaError> {
    match status["type"].as_str() {
        Some("configured") => Ok(()),
        Some("error") => Err(MediaError::Unavailable(
            status["code"]
                .as_str()
                .filter(|code| {
                    code.len() <= 64
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                .ok_or(MediaError::Protocol)?
                .to_owned(),
        )),
        _ => Err(MediaError::Protocol),
    }
}

// 宽高可交换，兼容竖屏；NV12 要求偶数尺寸，画面不能超出长/短边预算。
fn valid_dimensions(width: Option<u64>, height: Option<u64>) -> bool {
    let (Some(w), Some(h)) = (width, height) else {
        return false;
    };
    w >= 2 && h >= 2 && w % 2 == 0 && h % 2 == 0 && w.max(h) <= 1920 && w.min(h) <= 1440
}
