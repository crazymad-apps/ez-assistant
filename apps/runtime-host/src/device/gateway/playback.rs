//! 播报预留的短期所有权：连接确认接纳 PCM 后才向调用方交付成功。

use std::time::Duration;

use crate::media_diagnostics::{correlation_id, timestamp_ms};
use assistant_runtime::ChannelOutputDispatchError;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::{CancellationToken, DropGuard};

use super::{
    ConnectionCommand, InteractionStateChanged, PlaybackOutput, PlaybackPreparation,
    PlaybackPreparationResult,
};

const PLAYBACK_COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

/// 拥有尚未交付的连接队列预留，不保存 Runtime 或设备的第二份权威状态。
/// 合成失败、确认超时、调用方取消或 Future 丢弃均由 guard 取消原槽位，不能留下空队首。
pub(super) struct PreparedPlayback {
    command: mpsc::Sender<ConnectionCommand>,
    cancellation: CancellationToken,
    release: DropGuard,
}

impl PreparedPlayback {
    pub(super) async fn reserve(
        command: mpsc::Sender<ConnectionCommand>,
        output_id: String,
        cancellation: CancellationToken,
    ) -> Result<Self, ChannelOutputDispatchError> {
        let release = cancellation.clone().drop_guard();
        let request_id = correlation_id(&output_id);
        if cancellation.is_cancelled() {
            return Err(ChannelOutputDispatchError::Cancelled);
        }
        let (response, accepted) = oneshot::channel();
        command
            .try_send(ConnectionCommand::PreparePlayback(PlaybackPreparation {
                output_id,
                cancellation: cancellation.clone(),
                response,
            }))
            .map_err(|_| ChannelOutputDispatchError::Unavailable)?;
        let result = queue_response(accepted, &cancellation).await;
        let label = match &result {
            Ok(PlaybackPreparationResult::Accepted) => "accepted",
            Ok(PlaybackPreparationResult::Interrupted) => "cancelled",
            Ok(PlaybackPreparationResult::CapacityExceeded) => "capacity_exceeded",
            Err(ChannelOutputDispatchError::Cancelled) => "cancelled",
            Err(ChannelOutputDispatchError::Unavailable) => "unavailable",
        };
        eprintln!(
            "event=playback_reservation ts_ms={} request={} result={}",
            timestamp_ms(),
            request_id,
            label
        );
        match result? {
            PlaybackPreparationResult::Accepted => Ok(Self {
                command,
                cancellation,
                release,
            }),
            PlaybackPreparationResult::Interrupted => Err(ChannelOutputDispatchError::Cancelled),
            PlaybackPreparationResult::CapacityExceeded => {
                Err(ChannelOutputDispatchError::Unavailable)
            }
        }
    }

    pub(super) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// 调用前已经预留但尚未交付；只有连接 owner 的接纳回执允许转移取消责任。
    /// 成功只证明当时进入播放队列，不证明网络传输或扬声器播放完成；失败不重放 PCM。
    pub(super) async fn attach(
        self,
        output: PlaybackOutput,
    ) -> Result<(), ChannelOutputDispatchError> {
        if self.cancellation.is_cancelled() {
            return Err(ChannelOutputDispatchError::Cancelled);
        }
        let (response, accepted) = oneshot::channel();
        let request_id = correlation_id(&output.output_id);
        self.command
            .try_send(ConnectionCommand::StartPlayback { output, response })
            .map_err(|_| ChannelOutputDispatchError::Unavailable)?;
        let result = queue_response(accepted, &self.cancellation).await;
        let label = match result {
            Ok(true) => "accepted",
            Ok(false) | Err(ChannelOutputDispatchError::Cancelled) => "cancelled",
            Err(ChannelOutputDispatchError::Unavailable) => "unavailable",
        };
        eprintln!(
            "event=playback_admission ts_ms={} request={} result={}",
            timestamp_ms(),
            request_id,
            label
        );
        if !result? {
            return Err(ChannelOutputDispatchError::Cancelled);
        }
        // 连接已拥有 PCM/取消令牌；方法返回或正常 Tool 完成不能取消已经成功交付的片段。
        self.release.disarm();
        Ok(())
    }

    pub(super) fn notify_unavailable(&self, state: InteractionStateChanged) {
        if !self.cancellation.is_cancelled() {
            let _ = self
                .command
                .try_send(ConnectionCommand::OutputUnavailable(state));
        }
    }
}

async fn queue_response<T>(
    response: oneshot::Receiver<T>,
    cancellation: &CancellationToken,
) -> Result<T, ChannelOutputDispatchError> {
    tokio::select! {
        biased;
        // 已形成的回执优先：短音频可能在调用方再次被调度前就已经发完并释放槽位。
        response = tokio::time::timeout(PLAYBACK_COMMAND_TIMEOUT, response) => response
            .map_err(|_| ChannelOutputDispatchError::Unavailable)?
            .map_err(|_| ChannelOutputDispatchError::Unavailable),
        () = cancellation.cancelled() => Err(ChannelOutputDispatchError::Cancelled),
    }
}

#[cfg(test)]
mod tests;
