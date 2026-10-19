//! 输入 helper 的唯一 I/O owner；取消读取后只允许关闭，失败时补发本控制可能按下的释放。
use super::input::{Input, Pressed};
use assistant_protocol::DesktopInput;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

pub(super) struct NativeInput {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    state: Input,
    possible: Pressed,
    helper: PathBuf,
}
impl NativeInput {
    pub async fn start(display: &str, binding: &str) -> Result<Self, &'static str> {
        let helper = super::super::helper_path().map_err(|_| "input_unavailable")?;
        Self::start_bound(helper, display, Some(binding)).await
    }
    #[cfg(test)]
    pub(super) async fn start_helper(helper: PathBuf, display: &str) -> Result<Self, &'static str> {
        Self::start_bound(helper, display, None).await
    }
    async fn start_bound(
        helper: PathBuf,
        display: &str,
        binding: Option<&str>,
    ) -> Result<Self, &'static str> {
        let mut command = native_command(&helper);
        command.args(["--input", display]);
        if let Some(binding) = binding {
            command.arg(binding);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "input_unavailable")?;
        let input = child.stdin.take().ok_or("input_unavailable")?;
        let output = child.stdout.take().ok_or("input_unavailable")?;
        let mut native = Self {
            child,
            input: Some(input),
            output: BufReader::new(output),
            state: Input::new(),
            possible: Pressed::default(),
            helper,
        };
        match timeout(Duration::from_secs(5), native.line()).await {
            Ok(Ok(line)) if line == "ready" => Ok(native),
            other => {
                let code = match other {
                    Ok(Ok(line)) if line == "error input_permission_required" => {
                        "input_permission_required"
                    }
                    _ => "input_unavailable",
                };
                native.stop().await;
                Err(code)
            }
        }
    }
    async fn line(&mut self) -> Result<String, &'static str> {
        // fill_buf/consume 限定响应长度；不读无界原生输出。
        let mut bytes = Vec::new();
        loop {
            let buffer = self.output.fill_buf().await.map_err(|_| "input_failed")?;
            if buffer.is_empty() {
                return Err("input_failed");
            }
            let count = buffer
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(buffer.len());
            if bytes.len() + count > 256 {
                return Err("input_failed");
            }
            bytes.extend_from_slice(&buffer[..count]);
            self.output.consume(count);
            if bytes.last() == Some(&b'\n') {
                return String::from_utf8(bytes)
                    .map(|v| v.trim().to_owned())
                    .map_err(|_| "input_failed");
            }
        }
    }
    pub async fn command(
        &mut self,
        sequence: u64,
        input: &DesktopInput,
    ) -> Result<(), &'static str> {
        let before = self.state.pressed.clone();
        let command = self.state.prepare(sequence, input)?;
        self.possible.keys.extend(before.keys);
        self.possible.buttons.extend(before.buttons);
        self.possible.keys.extend(self.state.pressed.keys.clone());
        self.possible
            .buttons
            .extend(self.state.pressed.buttons.clone());
        if let Some(command) = command {
            let result = timeout(Duration::from_millis(250), async {
                self.input
                    .as_mut()
                    .ok_or("input_failed")?
                    .write_all(format!("{command}\n").as_bytes())
                    .await
                    .map_err(|_| "input_failed")?;
                if self.line().await? != format!("ok {sequence}") {
                    return Err("input_failed");
                }
                Ok(())
            })
            .await
            .map_err(|_| "input_stalled")?;
            result?;
        }
        self.possible = self.state.pressed.clone();
        Ok(())
    }
    /// 先结束执行器并等待其松键；异常退出才补发释放，不重试点击、文字或其他副作用。
    pub async fn stop(mut self) {
        if let Some(mut input) = self.input.take() {
            let _ = timeout(Duration::from_millis(100), input.write_all(b"stop 0\n")).await;
        }
        let clean = match timeout(Duration::from_secs(2), self.child.wait()).await {
            Ok(Ok(status)) => status.success(),
            _ => {
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
                false
            }
        };
        if !clean && (!self.possible.keys.is_empty() || !self.possible.buttons.is_empty()) {
            let keys = self.possible.keys.into_iter().collect::<Vec<_>>().join(",");
            let buttons = self
                .possible
                .buttons
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",");
            if let Ok(mut release) = native_command(&self.helper)
                .args(["--release", &keys, &buttons])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                match timeout(Duration::from_secs(2), release.wait()).await {
                    Ok(Ok(status)) if status.success() => {}
                    _ => {
                        let _ = release.kill().await;
                        let _ = release.wait().await;
                        eprintln!("runtime-host: desktop input release could not be confirmed");
                    }
                }
            } else {
                eprintln!("runtime-host: desktop input release helper unavailable");
            }
        }
    }
}

fn native_command(helper: &std::path::Path) -> Command {
    let mut command = Command::new(helper);
    command.env_clear();
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
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}
