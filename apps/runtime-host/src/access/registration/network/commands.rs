//! 本机 HTTP 用户意图到当前控制连接；不接受 UI 传入中心 Token。
use super::{
    Network,
    session::{LeaseGuard, Session, Source, Work},
};
use crate::access::{AccessError, AccessPermit};
use assistant_protocol::{HostNetworkCommand as Command, HostNetworkResult as ResultDto};
use serde_json::json;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
impl Network {
    pub(super) async fn connection(
        &self,
        permit: &AccessPermit,
    ) -> Result<Arc<Session>, AccessError> {
        self.demand.store(true, Ordering::Relaxed);
        self.refresh.notify_one();
        let mut connected = self.connected.subscribe();
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if let Some(session) = connected.borrow_and_update().clone() && !session.ended.is_cancelled() { return Ok(session); }
                tokio::select! {
                    () = permit.ended() => return Err(AccessError::Unauthorized),
                    result = connected.changed() => { result.map_err(|_| AccessError::Unavailable)?; },
                }
            }
        }).await.map_err(|_| AccessError::Unavailable)?
    }
    pub(crate) async fn command(
        &self,
        command: Command,
        permit: AccessPermit,
    ) -> Result<ResultDto, AccessError> {
        permit.check()?;
        let key = permit.user_key().ok_or(AccessError::Unauthorized)?;
        let (_, token, _) = self.credentials.current_agent_center(key)?;
        let session = self.connection(&permit).await?;
        let close = matches!(&command, Command::CloseAccess { .. });
        match command {
            Command::ListDevices { offset } => {
                let value = session
                    .request("directory", json!({"token":token.expose(),"offset":offset}))
                    .await?;
                #[derive(serde::Deserialize)]
                struct Directory {
                    items: Vec<assistant_protocol::HostNetworkDevice>,
                    next_offset: Option<u32>,
                }
                let directory: Directory =
                    serde_json::from_value(value).map_err(|_| AccessError::Unavailable)?;
                permit.check()?;
                Ok(ResultDto::Devices {
                    self_device_id: session.self_device_id.clone(),
                    items: directory.items,
                    next_offset: directory.next_offset,
                })
            }
            Command::OpenAccess {
                device_id,
                capability,
            } => {
                if !super::super::store::valid_id(&device_id) {
                    return Err(AccessError::Invalid("设备编号无效。"));
                }
                let (id, _) = session
                    .open_source(device_id.clone(), capability, permit.clone(), false)
                    .await?;
                Ok(ResultDto::Access {
                    access_id: id,
                    device_id,
                    capability,
                    active: true,
                })
            }
            Command::GetAccess { access_id } | Command::CloseAccess { access_id } => {
                let source = session
                    .sources
                    .lock()
                    .expect("network sources poisoned")
                    .get(&access_id)
                    .cloned();
                let Some(source) = source else {
                    return Ok(ResultDto::Closed);
                };
                if source.permit.login_id() != permit.login_id() {
                    return Err(AccessError::Unauthorized);
                }
                if close {
                    session.close_access(&access_id);
                    return Ok(ResultDto::Closed);
                }
                Ok(ResultDto::Access {
                    access_id,
                    device_id: source.device,
                    capability: source.capability,
                    active: matches!(*source.ready.borrow(), Some(Ok(())))
                        && !source.cancelled.is_cancelled()
                        && source.permit.check().is_ok(),
                })
            }
        }
    }
}

impl Session {
    pub(super) async fn open_source(
        self: &Arc<Self>,
        device: String,
        capability: assistant_protocol::HostNetworkCapability,
        permit: AccessPermit,
        reuse: bool,
    ) -> Result<(String, Source), AccessError> {
        self.open_source_bound(device, capability, permit, reuse, None)
            .await
    }
    pub(super) async fn open_source_bound(
        self: &Arc<Self>,
        device: String,
        capability: assistant_protocol::HostNetworkCapability,
        permit: AccessPermit,
        reuse: bool,
        view_id: Option<String>,
    ) -> Result<(String, Source), AccessError> {
        let (id, source, mut ready) = {
            let mut sources = self.sources.lock().expect("network sources poisoned");
            if let Some((id, source)) = sources.iter().find(|(_, source)| {
                reuse
                    && source.device == device
                    && source.capability == capability
                    && source.permit.login_id() == permit.login_id()
                    && !source.cancelled.is_cancelled()
                    && source.permit.check().is_ok()
            }) {
                (id.clone(), source.clone(), source.ready.subscribe())
            } else {
                let id = super::super::store::new_id()?;
                let (ready, receiver) = tokio::sync::watch::channel(None);
                let source = Source {
                    permit,
                    device,
                    view_id,
                    capability,
                    cancelled: self.ended.child_token(),
                    ready,
                };
                if sources.len() >= 64 {
                    return Err(AccessError::Busy);
                }
                // 与 install_source 一样受 leases 上限约束；不持锁调用会反向取得 sources 的收尾。
                let mut leases = self.leases.lock().expect("network leases poisoned");
                if leases.len() >= 64 {
                    return Err(AccessError::Busy);
                }
                leases.insert(id.clone(), source.cancelled.clone());
                sources.insert(id.clone(), source.clone());
                drop(leases);
                drop(sources);
                if capability != assistant_protocol::HostNetworkCapability::Ordinary {
                    self.create_desktop(&id);
                }
                let guard = LeaseGuard {
                    session: self.clone(),
                    id: id.clone(),
                };
                self.jobs
                    .try_send(Work::Opening(id.clone(), Box::new(source.clone()), guard))
                    .map_err(|_| AccessError::Busy)?;
                (id, source, receiver)
            }
        };
        loop {
            let outcome = *ready.borrow_and_update();
            if let Some(outcome) = outcome {
                outcome.map_err(super::session::SourceFailure::error)?;
                source.permit.check()?;
                return Ok((id, source));
            }
            tokio::select! {
                () = source.cancelled.cancelled() => {
                    if let Some(Err(error)) = *ready.borrow() { return Err(error.error()); }
                    return Err(AccessError::NetworkEnded);
                },
                () = self.ended.cancelled() => return Err(AccessError::Unavailable),
                () = source.permit.ended() => return Err(AccessError::Unauthorized),
                result = ready.changed() => { result.map_err(|_| AccessError::NetworkEnded)?; }
            }
        }
    }
}
impl Network {
    pub(crate) async fn open_flow(
        &self,
        device: String,
        permit: AccessPermit,
        kind: super::streams::Kind,
    ) -> Result<super::streams::Flow, AccessError> {
        let session = self.connection(&permit).await?;
        let (id, source) = session
            .open_source(
                device,
                assistant_protocol::HostNetworkCapability::Ordinary,
                permit,
                true,
            )
            .await?;
        session
            .streams
            .open(id, source.cancelled, kind)
            .map_err(|_| AccessError::Busy)
    }
}
