//! Supervisor 拥有全部桌面任务；中心任务只提交，不因连接 future 被替换而跳过媒体关闭。
use super::{MediaError, peer::stun_servers};
use crate::access::registration::network::DesktopAccess;
use futures_util::future::BoxFuture;
use std::sync::Arc;
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use webrtc::peer_connection::RTCIceServer;

type Task = BoxFuture<'static, Result<(), MediaError>>;
pub(crate) struct DesktopViews {
    tasks: mpsc::Sender<Task>,
    pub(super) controls: super::control::Controls,
    pub(crate) sockets: Arc<Semaphore>,
    targets: Arc<Semaphore>,
    pub(super) captures: super::shared::Captures,
    pub(super) shutdown: CancellationToken,
    pub(super) stun: Vec<RTCIceServer>,
}
pub(crate) struct DesktopService {
    pub(crate) handle: Arc<DesktopViews>,
    tasks: mpsc::Receiver<Task>,
}
impl DesktopService {
    pub(crate) fn new() -> Result<Self, MediaError> {
        let stun = stun_servers(&std::env::var("EZ_ASSISTANT_DESKTOP_STUN").unwrap_or_default())?;
        let (send, tasks) = mpsc::channel(12);
        Ok(Self {
            handle: Arc::new(DesktopViews {
                tasks: send,
                controls: Default::default(),
                sockets: Arc::new(Semaphore::new(8)),
                targets: Arc::new(Semaphore::new(4)),
                captures: Default::default(),
                shutdown: CancellationToken::new(),
                stun,
            }),
            tasks,
        })
    }
    pub(crate) async fn run_until(mut self, shutdown: CancellationToken) -> Result<(), MediaError> {
        let mut running = JoinSet::new();
        loop {
            tokio::select! { biased;
                () = shutdown.cancelled() => break,
                completed = running.join_next(), if !running.is_empty() => observe(completed),
                task = self.tasks.recv() => match task { Some(task) => { running.spawn(task); }, None => break },
            }
        }
        self.handle.shutdown.cancel();
        self.tasks.close();
        while let Some(task) = self.tasks.recv().await {
            running.spawn(task);
        }
        while let Some(result) = running.join_next().await {
            observe(Some(result));
        }
        Ok(())
    }
}
fn observe(result: Option<Result<Result<(), MediaError>, tokio::task::JoinError>>) {
    if let Some(Err(_)) = result {
        eprintln!("runtime-host: desktop task failed");
    }
    if let Some(Ok(Err(error))) = result {
        let reason = match error {
            MediaError::Protocol => "invalid_protocol",
            MediaError::Transport(_) => "transport_failure",
            MediaError::Access(_) => "access_ended",
            MediaError::Ended => "connection_ended",
            MediaError::ConnectTimeout => "connect_timeout",
            MediaError::MissingHelper => "missing_helper",
            MediaError::UnsupportedPlatform => "unsupported_platform",
            MediaError::Io(_) => "capture_io",
            MediaError::Unavailable(_) => "capture_unavailable",
            MediaError::StopTimeout => "stop_timeout",
            MediaError::NoFrames => "no_frames",
        };
        eprintln!("runtime-host: desktop connection ended: {reason}");
    }
}
impl DesktopViews {
    pub(crate) fn submit(&self, task: Task) -> Result<(), MediaError> {
        if self.shutdown.is_cancelled() {
            return Err(MediaError::Ended);
        }
        self.tasks.try_send(task).map_err(|_| MediaError::Ended)
    }
    pub(crate) fn submit_target(
        self: &Arc<Self>,
        access: DesktopAccess,
        identity: super::control::Identity,
    ) -> Result<(), MediaError> {
        let slot = self
            .targets
            .clone()
            .try_acquire_owned()
            .map_err(|_| MediaError::Unavailable("desktop_busy".into()))?;
        let owner = self.clone();
        self.submit(Box::pin(async move {
            let _slot = slot;
            super::target::serve(access, owner, identity).await
        }))
    }
}
