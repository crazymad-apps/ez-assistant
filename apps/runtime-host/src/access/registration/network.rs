//! 设备连接的进程级 owner。凭据快照只用于当前传输，持久登记仍由 Access actor 修改。
mod commands;
mod desktop;
pub(crate) use desktop::DesktopAccess;
mod connection;
mod session;
pub(crate) mod streams;
#[cfg(test)]
mod tests;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use assistant_protocol::{HostMode, HostNetworkState, HostRegistrationIntent as Intent};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::{Enrollment, Store, check_reply};
use crate::access::{AccessError, HostAccessService, enterprise::CenterClient};

struct Target {
    client: CenterClient,
    enrollment: Option<Enrollment>,
    center_id: String,
}
impl Target {
    fn enrollment(&self) -> Option<&Enrollment> {
        self.enrollment.as_ref()
    }
    fn same(&self, other: &Self) -> bool {
        self.client.url == other.client.url
            && self.center_id == other.center_id
            && match (self.enrollment(), other.enrollment()) {
                (Some(a), Some(b)) => a.id == b.id && a.secret.expose() == b.secret.expose(),
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Clone)]
pub(crate) struct Network {
    credentials: Arc<crate::access::Credentials>,
    demand: Arc<AtomicBool>,
    connected: watch::Sender<Option<Arc<session::Session>>>,
    desired: watch::Sender<Option<Arc<Target>>>,
    pub(in crate::access) status: watch::Sender<HostNetworkState>,
    pub(in crate::access) refresh: Arc<Notify>,
}
impl Network {
    pub(in crate::access) fn new(credentials: Arc<crate::access::Credentials>) -> Self {
        Self {
            credentials,
            demand: Arc::new(AtomicBool::new(false)),
            connected: watch::channel(None).0,
            desired: watch::channel(None).0,
            status: watch::channel(HostNetworkState::Disconnected).0,
            refresh: Arc::new(Notify::new()),
        }
    }
    pub(in crate::access) fn clear(&self) {
        self.publish(None);
    }
    fn publish(&self, target: Option<Target>) {
        self.desired.send_if_modified(|current| {
            if match (current.as_ref(), target.as_ref()) {
                (Some(a), Some(b)) => a.same(b),
                (None, None) => true,
                _ => false,
            } {
                return false;
            }
            *current = target.map(Arc::new);
            true
        });
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<HostNetworkState> {
        self.status.subscribe()
    }
    pub(crate) async fn run_until(
        self,
        shutdown: CancellationToken,
        state: Option<crate::http::HttpState>,
    ) -> Result<(), AccessError> {
        let _status_guard = ConnectionState(self.status.clone());
        let mut desired = self.desired.subscribe();
        loop {
            let target = desired.borrow_and_update().clone();
            self.status.send_replace(HostNetworkState::Disconnected);
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                _ = desired.changed() => {},
                () = async {
                    if let Some(target) = target { self.connect_loop(target, state.clone()).await; }
                    else { std::future::pending::<()>().await; }
                } => {},
            }
        }
        self.status.send_replace(HostNetworkState::Disconnected);
        Ok(())
    }
    async fn connect_loop(&self, target: Arc<Target>, state: Option<crate::http::HttpState>) {
        let mut failures = 0;
        loop {
            self.status.send_replace(HostNetworkState::Connecting);
            self.refresh.notify_one();
            let (result, stable) = connection::run(&target, self, state.clone()).await;
            self.status.send_replace(result);
            self.refresh.notify_one();
            if result == HostNetworkState::Incompatible {
                std::future::pending::<()>().await;
            }
            if stable {
                failures = 0;
            }
            let seconds = backoff(failures);
            failures = failures.saturating_add(1);
            let mut jitter = [0; 2];
            let _ = getrandom::fill(&mut jitter);
            tokio::time::sleep(
                Duration::from_secs(seconds)
                    + Duration::from_millis(u64::from(u16::from_le_bytes(jitter) % 251)),
            )
            .await;
        }
    }
}
// Supervisor 中断或任务异常同样清除本机在线投影；socket 随 owner future 释放。
struct ConnectionState(watch::Sender<HostNetworkState>);
impl Drop for ConnectionState {
    fn drop(&mut self) {
        self.0.send_replace(HostNetworkState::Disconnected);
    }
}
fn backoff(failures: u32) -> u64 {
    1u64.checked_shl(failures).unwrap_or(30).min(30)
}

impl HostAccessService {
    /// 只在 actor 中读取/写入登记；连接回调只请求刷新，不携带可覆盖新身份的结果。
    pub(in crate::access) async fn publish_network(&self) -> Result<(), AccessError> {
        let document = super::super::config::load(self.source.as_ref()).await?;
        let host = crate::host_configuration::parse(&document.document.to_string())
            .map_err(AccessError::Invalid)?;
        let store = Store::open(self.source.as_ref()).await?;
        let client = self
            .center
            .read()
            .map_err(|_| AccessError::Unavailable)?
            .clone();
        let configured = host
            .enterprise
            .as_ref()
            .and_then(|e| e.center_id.as_deref())
            .unwrap_or_default();
        let target = if host.mode == HostMode::Enterprise && !self.identity_restart_required {
            store
                .identity
                .and_then(|mut identity| identity.registrations.remove(configured))
                .filter(|enrollment| enrollment.intent == Intent::Publish)
                .zip(client.clone())
                .map(|(enrollment, client)| Target {
                    center_id: enrollment.center_id.clone(),
                    client,
                    enrollment: Some(enrollment),
                })
                .or_else(|| {
                    let center_id = host.enterprise.as_ref()?.center_id.clone()?;
                    if !self.network.demand.load(Ordering::Relaxed)
                        || self.credentials.network_caller(&center_id).is_none()
                    {
                        return None;
                    }
                    Some(Target {
                        client: client?,
                        enrollment: None,
                        center_id,
                    })
                })
        } else {
            None
        };
        self.network.publish(target);
        Ok(())
    }
    pub(in crate::access) async fn refresh_network(&self) {
        if self.synchronize_network().await.is_err() {
            // 中心暂不可达不删除已保存的身份；本地材料损坏则明确停止连接。
            if self.publish_network().await.is_err() {
                self.network.clear();
                self.network.status.send_replace(HostNetworkState::Failed);
            }
        }
    }
    async fn synchronize_network(&self) -> Result<(), AccessError> {
        self.publish_network().await?;
        let document = super::super::config::load(self.source.as_ref()).await?;
        let host = crate::host_configuration::parse(&document.document.to_string())
            .map_err(AccessError::Invalid)?;
        let mut store = Store::open(self.source.as_ref()).await?;
        let configured = host
            .enterprise
            .as_ref()
            .and_then(|e| e.center_id.as_deref())
            .unwrap_or_default();
        let Some(identity) = store.identity.as_ref() else {
            return Ok(());
        };
        let Some(e) = identity.registrations.get(configured) else {
            return Ok(());
        };
        if e.intent == Intent::Closed
            || host.mode != HostMode::Enterprise
            || self.identity_restart_required
            || host
                .enterprise
                .as_ref()
                .and_then(|c| c.center_id.as_deref())
                != Some(&e.center_id)
        {
            return Ok(());
        }
        let client = self
            .center
            .read()
            .map_err(|_| AccessError::Unavailable)?
            .clone()
            .ok_or(AccessError::Unavailable)?;
        let mut reply = client
            .registration(&e.center_id, &e.id, &e.secret, None)
            .await?;
        check_reply(&reply, identity, e)?;
        if reply
            .device
            .as_ref()
            .is_some_and(|d| !d.status.is_terminal())
            && matches!(e.intent, Intent::Withdraw | Intent::Exit)
        {
            reply = client
                .registration(
                    &e.center_id,
                    &e.id,
                    &e.secret,
                    Some(if e.intent == Intent::Exit {
                        "exit"
                    } else {
                        "withdraw"
                    }),
                )
                .await?;
            check_reply(&reply, identity, e)?;
        }
        if reply
            .device
            .as_ref()
            .is_some_and(|d| d.status.is_terminal())
        {
            store
                .identity
                .as_mut()
                .and_then(|i| i.registrations.get_mut(configured))
                .ok_or(AccessError::Unavailable)?
                .intent = Intent::Closed;
            store.save().await?;
        }
        self.publish_network().await
    }
}
