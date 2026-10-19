//! 控制许可绑定现有查看；目标持有唯一占用，原生执行器只缓存本控制者的已按下集合。
mod input;
mod native;
pub(super) mod source;
mod target;
#[cfg(test)]
mod tests;

use super::{MediaError, service::DesktopViews};
use crate::access::registration::network::DesktopAccess;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc};
use webrtc::data_channel::{DataChannel, DataChannelEvent};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) source: String,
    pub(crate) user: i32,
}
struct View {
    identity: Identity,
    send: mpsc::Sender<DesktopAccess>,
}
pub(crate) struct Controls {
    views: Mutex<HashMap<String, View>>,
    exclusive: Arc<Semaphore>,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            views: Mutex::new(HashMap::new()),
            exclusive: Arc::new(Semaphore::new(1)),
        }
    }
}
pub(super) struct ViewRegistration {
    id: String,
    owner: Arc<DesktopViews>,
}
impl Drop for ViewRegistration {
    fn drop(&mut self) {
        self.owner
            .controls
            .views
            .lock()
            .expect("desktop views poisoned")
            .remove(&self.id);
    }
}
impl DesktopViews {
    pub(super) fn register_view(
        self: &Arc<Self>,
        id: String,
        identity: Identity,
    ) -> (ViewRegistration, mpsc::Receiver<DesktopAccess>) {
        let (send, receive) = mpsc::channel(4);
        self.controls
            .views
            .lock()
            .expect("desktop views poisoned")
            .insert(id.clone(), View { identity, send });
        (
            ViewRegistration {
                id,
                owner: self.clone(),
            },
            receive,
        )
    }
    /// 企业控制许可已核验，只能交给同一中心连接、用户和查看绑定；不跨查看抢占。
    pub(crate) fn submit_control(
        &self,
        view: &str,
        identity: Identity,
        access: DesktopAccess,
    ) -> Result<(), MediaError> {
        let send = self
            .controls
            .views
            .lock()
            .expect("desktop views poisoned")
            .get(view)
            .filter(|v| v.identity == identity)
            .map(|v| v.send.clone())
            .ok_or(MediaError::Ended)?;
        send.try_send(access).map_err(|_| MediaError::Ended)
    }
}
pub(super) use target::{State, serve};
pub(super) async fn send(
    channel: &Arc<dyn DataChannel>,
    value: serde_json::Value,
) -> Result<(), MediaError> {
    let text = value.to_string();
    if text.len() > 4096 {
        return Err(MediaError::Protocol);
    }
    tokio::time::timeout(Duration::from_millis(250), channel.try_send_text(&text))
        .await
        .map_err(|_| MediaError::Ended)??;
    Ok(())
}
pub(super) fn message(
    event: Option<DataChannelEvent>,
) -> Result<Option<serde_json::Value>, MediaError> {
    match event {
        Some(DataChannelEvent::OnMessage(m)) if m.is_string && m.data.len() <= 4096 => {
            serde_json::from_slice(&m.data)
                .map(Some)
                .map_err(|_| MediaError::Protocol)
        }
        Some(
            DataChannelEvent::OnOpen
            | DataChannelEvent::OnBufferedAmountLow
            | DataChannelEvent::OnBufferedAmountHigh,
        ) => Ok(None),
        _ => Err(MediaError::Ended),
    }
}
