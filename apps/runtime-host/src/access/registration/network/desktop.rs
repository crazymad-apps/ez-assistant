//! 桌面协商只消费已存在的访问许可；媒体字节从不进入中心连接。
use super::{
    Network,
    session::{Session, Source},
};
use crate::access::{AccessError, AccessPermit};
use assistant_protocol::HostNetworkCapability;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) struct DesktopAccess {
    session: Arc<Session>,
    pub(crate) id: String,
    pub(crate) messages: mpsc::Receiver<Value>,
    pub(crate) cancelled: CancellationToken,
    pub(crate) permit: AccessPermit,
}
impl DesktopAccess {
    /// 在同一连接和原登录下申请独立控制许可；视图取消会取消等待并由中心级联关闭控制。
    pub(crate) fn control_opener(
        &self,
    ) -> Box<dyn Fn() -> futures_util::future::BoxFuture<'static, Result<Self, AccessError>> + Send>
    {
        let session = self.session.clone();
        let view = self.id.clone();
        let cancelled = self.cancelled.clone();
        Box::new(move || {
            let session = session.clone();
            let view = view.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move {
                let source = session
                    .sources
                    .lock()
                    .expect("network sources poisoned")
                    .get(&view)
                    .cloned()
                    .ok_or(AccessError::NetworkEnded)?;
                let (id, source) = tokio::select! {
                    () = cancelled.cancelled() => return Err(AccessError::NetworkEnded),
                    result = session.open_source_bound(source.device, HostNetworkCapability::DesktopControl, source.permit, false, Some(view)) => result?,
                };
                session.desktop_access(id, source.permit, source.cancelled)
            })
        })
    }

    pub(crate) async fn send(&self, signal: Value) -> Result<(), AccessError> {
        self.permit.check()?;
        if self.cancelled.is_cancelled() {
            return Err(AccessError::NetworkEnded);
        }
        self.session
            .request(
                "desktop_signal",
                json!({"access_id":self.id,"signal":signal}),
            )
            .await?;
        Ok(())
    }
}
impl Drop for DesktopAccess {
    fn drop(&mut self) {
        self.session.close_access(&self.id);
    }
}

pub(super) struct Inbox {
    send: mpsc::Sender<Value>,
    receive: Option<mpsc::Receiver<Value>>,
}
impl Session {
    pub(super) fn create_desktop(&self, id: &str) {
        let (send, receive) = mpsc::channel(16);
        self.desktop.lock().expect("desktop inbox poisoned").insert(
            id.to_owned(),
            Inbox {
                send,
                receive: Some(receive),
            },
        );
    }
    pub(super) fn receive_desktop(&self, value: Value) -> Result<(), AccessError> {
        let id = value["access_id"]
            .as_str()
            .ok_or(AccessError::Unavailable)?;
        let delivered = {
            let inboxes = self.desktop.lock().expect("desktop inbox poisoned");
            // 已撤销访问的迟到信令不恢复资源，也不破坏其他普通业务连接。
            let Some(inbox) = inboxes.get(id) else {
                return Ok(());
            };
            inbox.send.try_send(value["signal"].clone()).is_ok()
        };
        if !delivered {
            self.close_access(id);
        }
        Ok(())
    }
    pub(super) fn desktop_access(
        self: &Arc<Self>,
        id: String,
        permit: AccessPermit,
        cancelled: CancellationToken,
    ) -> Result<DesktopAccess, AccessError> {
        let messages = self
            .desktop
            .lock()
            .expect("desktop inbox poisoned")
            .get_mut(&id)
            .and_then(|inbox| inbox.receive.take())
            .ok_or(AccessError::NetworkEnded)?;
        Ok(DesktopAccess {
            session: self.clone(),
            id,
            messages,
            cancelled,
            permit,
        })
    }
}
impl Network {
    pub(crate) async fn open_desktop(
        &self,
        device: String,
        permit: AccessPermit,
    ) -> Result<DesktopAccess, AccessError> {
        if !super::super::store::valid_id(&device) {
            return Err(AccessError::Invalid("设备编号无效。"));
        }
        let session = self.connection(&permit).await?;
        let (
            id,
            Source {
                cancelled, permit, ..
            },
        ) = session
            .open_source(device, HostNetworkCapability::DesktopView, permit, false)
            .await?;
        session.desktop_access(id, permit, cancelled)
    }
}
