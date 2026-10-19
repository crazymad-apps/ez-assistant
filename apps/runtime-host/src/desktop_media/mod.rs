//! Host 私有桌面采集适配。只拥有短期原生子进程与编码帧，不持有 Session/Run 或企业权限。
//! M0 由本机显式诊断入口消费；正式远端访问在 M1 获准后绑定同一 owner 的取消生命周期。

mod capture;
pub(crate) mod control;
mod feedback;
mod peer;
pub(crate) mod service;
mod shared;
pub(crate) mod socket;
mod target;

use clap::Args;
use std::{path::PathBuf, time::Duration};
use tokio::time::{Instant, timeout_at};

use capture::Capture;
pub(super) use capture::MediaError;

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub(crate) struct Diagnostics {
    /// Omit to enumerate displays without starting capture.
    #[arg(long)]
    display: Option<String>,
    /// Capture only a bounded diagnostic interval; no video is persisted.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..=10))]
    seconds: u64,
}

fn helper_path() -> Result<PathBuf, MediaError> {
    if !cfg!(any(target_os = "macos", target_os = "windows")) {
        return Err(MediaError::UnsupportedPlatform);
    }
    let exe = std::env::current_exe()?;
    Ok(exe.with_file_name(if cfg!(windows) {
        "ez-assistant-desktop-capture.exe"
    } else {
        "ez-assistant-desktop-capture"
    }))
}

/// 本机用户主动执行的短诊断，不启动 Runtime、不读取用户库、不提供远程访问或系统输入。
pub(crate) async fn diagnose(options: Diagnostics) -> Result<(), MediaError> {
    let helper = helper_path()?;
    if let Some(display) = options.display {
        let mut capture = Capture::start(&helper, &display).await?;
        let format = capture.format.clone();
        let deadline = Instant::now() + Duration::from_secs(options.seconds);
        let mut frames = 0;
        let mut keyframes = 0;
        let mut bytes = 0;
        let result = async {
            loop {
                match timeout_at(deadline, capture.next_frame()).await {
                    Ok(Ok(frame)) => {
                        frames += 1;
                        keyframes += u64::from(frame.keyframe);
                        bytes += frame.data.len();
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(_) => break,
                }
            }
            if frames == 0 {
                return Err(MediaError::NoFrames);
            }
            Ok(())
        }
        .await;
        // 任何读取错误/超时都先结束子进程，再返回诊断结果；不因 `?` 跳过回收。
        let cleanup = capture.stop().await;
        result?;
        cleanup?;
        println!(
            "{}",
            serde_json::json!({"format": format, "frames": frames, "keyframes": keyframes, "encoded_bytes": bytes})
        );
    } else {
        println!("{}", capture::displays(&helper).await?);
    }
    Ok(())
}
