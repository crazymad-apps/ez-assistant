//! 单条控制连接内的有界请求及访问生命周期；连接 owner 统一轮询所有 future。
#[cfg(test)]
mod tests;
use super::streams::{Flow, Streams};
use crate::access::{
    AccessError, AccessPermit, Credentials,
    enterprise::{CenterClient, UserKey},
};
use crate::user_domain::UserDomains;
use assistant_protocol::HostNetworkCapability;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

type Reply = oneshot::Sender<Result<Value, AccessError>>;
#[derive(Clone)]
pub(super) struct Source {
    pub permit: AccessPermit,
    pub device: String,
    pub view_id: Option<String>,
    pub capability: HostNetworkCapability,
    pub cancelled: CancellationToken,
    pub ready: watch::Sender<Option<Result<(), SourceFailure>>>,
}
#[derive(Clone, Copy)]
pub(super) enum SourceFailure {
    Denied,
    Busy,
    Ended,
}
impl SourceFailure {
    pub fn error(self) -> AccessError {
        match self {
            Self::Denied => AccessError::NetworkDenied,
            Self::Busy => AccessError::Busy,
            Self::Ended => AccessError::NetworkEnded,
        }
    }
}
pub(super) struct Session {
    /// 本次已协商连接的身份快照，不能随后续登记配置变化而替换。
    pub self_device_id: Option<String>,
    send: mpsc::Sender<Message>,
    pub jobs: mpsc::Sender<Work>,
    pub ended: CancellationToken,
    pending: Mutex<HashMap<String, Reply>>,
    pub(super) leases: Mutex<HashMap<String, CancellationToken>>,
    pub sources: Mutex<HashMap<String, Source>>,
    pub credentials: Arc<Credentials>,
    pub streams: Arc<Streams>,
    pub(super) desktop: Mutex<HashMap<String, super::desktop::Inbox>>,
    targets: Mutex<HashMap<String, (AccessPermit, HostNetworkCapability)>>,
}
#[derive(Deserialize)]
pub(super) struct Offer {
    access_id: String,
    user_id: i32,
    source_connection_id: Option<String>,
    view_id: Option<String>,
    capability: HostNetworkCapability,
    valid_for_ms: u64,
}
pub(super) enum Work {
    Target(Offer),
    #[cfg(test)]
    Source(String, Box<Source>, LeaseGuard, Instant),
    Opening(String, Box<Source>, LeaseGuard),
    Proxy(Flow, AccessPermit, super::streams::Kind),
}
impl Session {
    pub fn new(
        send: mpsc::Sender<Message>,
        credentials: Arc<Credentials>,
        self_device_id: Option<String>,
    ) -> (Arc<Self>, mpsc::Receiver<Work>) {
        let (jobs, receive) = mpsc::channel(64);
        (
            Arc::new(Self {
                self_device_id,
                send,
                jobs,
                ended: CancellationToken::new(),
                pending: Mutex::new(HashMap::new()),
                leases: Mutex::new(HashMap::new()),
                sources: Mutex::new(HashMap::new()),
                credentials,
                streams: Streams::new(),
                targets: Mutex::new(HashMap::new()),
                desktop: Mutex::new(HashMap::new()),
            }),
            receive,
        )
    }
    fn send(&self, value: Value) -> Result<(), AccessError> {
        let text = value.to_string();
        if self.ended.is_cancelled() || text.len() > 16384 {
            return Err(AccessError::Unavailable);
        }
        self.send
            .try_send(Message::Text(text.into()))
            .map_err(|_| AccessError::Busy)
    }
    pub async fn request(&self, action: &str, payload: Value) -> Result<Value, AccessError> {
        let id = super::super::store::new_id()?;
        let (send, receive) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("network requests poisoned");
            if pending.len() >= 16 {
                return Err(AccessError::Busy);
            }
            pending.insert(id.clone(), send);
        }
        let _guard = RequestGuard {
            session: self,
            id: &id,
        };
        self.send(json!({"type":"request", "request_id":id, "action":action, "payload":payload}))?;
        tokio::select! {
            () = self.ended.cancelled() => Err(AccessError::Unavailable),
            result = timeout(Duration::from_secs(if action == "open" { 35 } else { 5 }), receive) => result.map_err(|_| AccessError::Unavailable)?.map_err(|_| AccessError::Unavailable)?,
        }
    }
    #[cfg(test)]
    pub fn install_source(&self, id: &str, source: Source) -> Result<(), AccessError> {
        let mut leases = self.leases.lock().expect("network leases poisoned");
        if leases.len() >= 64 {
            drop(leases);
            self.close_access(id);
            return Err(AccessError::Busy);
        }
        leases.insert(id.to_owned(), source.cancelled.clone());
        self.sources
            .lock()
            .expect("network sources poisoned")
            .insert(id.to_owned(), source);
        Ok(())
    }
    pub fn close_access(&self, id: &str) {
        self.desktop
            .lock()
            .expect("desktop inbox poisoned")
            .remove(id);
        self.streams.revoke(id);
        self.targets
            .lock()
            .expect("network targets poisoned")
            .remove(id);
        if let Some(cancelled) = self
            .leases
            .lock()
            .expect("network leases poisoned")
            .remove(id)
        {
            cancelled.cancel();
        }
        self.sources
            .lock()
            .expect("network sources poisoned")
            .remove(id);
        // 只发一次幂等收尾；断线时中心的 connection owner 承担同一收尾。
        if self.send(json!({"type":"request", "request_id":super::super::store::new_id().unwrap_or_default(), "action":"close", "payload":{"access_id":id}})).is_err() { self.ended.cancel(); }
    }
    pub fn close(&self) {
        self.desktop.lock().expect("desktop inbox poisoned").clear();
        self.streams.close();
        self.targets
            .lock()
            .expect("network targets poisoned")
            .clear();
        self.ended.cancel();
        for (_, token) in self.leases.lock().expect("network leases poisoned").drain() {
            token.cancel();
        }
        self.sources
            .lock()
            .expect("network sources poisoned")
            .clear();
        self.pending
            .lock()
            .expect("network requests poisoned")
            .clear();
    }
    pub fn receive(&self, value: Value, target: bool) -> Result<(), AccessError> {
        if value["type"]
            .as_str()
            .is_some_and(|v| v.starts_with("stream_"))
        {
            if value["type"] == "stream_open" {
                let (id, stream) =
                    super::streams::control_key(&value).map_err(|_| AccessError::Unavailable)?;
                let kind = serde_json::from_value::<super::streams::Kind>(value["kind"].clone())
                    .map_err(|_| AccessError::NetworkDenied)?;
                let context = self
                    .targets
                    .lock()
                    .expect("network targets poisoned")
                    .get(&id)
                    .cloned();
                let cancelled = self
                    .leases
                    .lock()
                    .expect("network leases poisoned")
                    .get(&id)
                    .cloned();
                let valid = context.filter(|(permit, capability)| {
                    permit.check().is_ok() && *capability == HostNetworkCapability::Ordinary
                });
                let (Some((permit, _)), Some(cancelled)) = (valid, cancelled) else {
                    // 目标本机退出可能先于中心的在途 open 到达；只拒绝这一条流，不冻结设备连接。
                    self.send(json!({"type":"stream_reset","access_id":id,"stream_id":stream.to_string(),"code":"network_access_denied"}))?;
                    return Ok(());
                };
                match self.streams.accept(id.clone(), stream, cancelled, kind) {
                    Ok(flow) => self.jobs.try_send(Work::Proxy(flow, permit, kind)).map_err(|_| AccessError::Busy)?,
                    Err(_) => self.send(json!({"type":"stream_reset","access_id":id,"stream_id":stream.to_string(),"code":"network_busy"}))?,
                }
            } else {
                self.streams
                    .control(&value)
                    .map_err(|_| AccessError::Unavailable)?;
            }
            return Ok(());
        }
        match value["type"].as_str() {
            Some("desktop_signal") => self.receive_desktop(value)?,
            Some("response") => {
                let id = value["request_id"]
                    .as_str()
                    .ok_or(AccessError::Unavailable)?;
                if let Some(reply) = self
                    .pending
                    .lock()
                    .expect("network requests poisoned")
                    .remove(id)
                {
                    let result = match value.get("error").and_then(Value::as_str) {
                        None => Ok(value["result"].clone()),
                        Some("TOKEN_INVALID") => Err(AccessError::Unauthorized),
                        Some("DEVICE_ACCESS_DENIED") => Err(AccessError::NetworkDenied),
                        Some("DEVICE_ACCESS_ENDED" | "DEVICE_CHANGED") => {
                            Err(AccessError::NetworkEnded)
                        }
                        Some("RATE_LIMITED") => Err(AccessError::Busy),
                        Some(_) => Err(AccessError::Unavailable),
                    };
                    let _ = reply.send(result);
                }
            }
            Some("access_offer") if target => {
                let offer: Offer =
                    serde_json::from_value(value).map_err(|_| AccessError::Unavailable)?;
                if !super::super::store::valid_id(&offer.access_id)
                    || !offer
                        .source_connection_id
                        .as_deref()
                        .is_none_or(super::super::store::valid_id)
                    || (offer.capability != HostNetworkCapability::Ordinary
                        && offer.source_connection_id.is_none())
                    || (offer.capability == HostNetworkCapability::DesktopControl
                        && !offer
                            .view_id
                            .as_deref()
                            .is_some_and(super::super::store::valid_id))
                    || offer.user_id <= 0
                    || offer.valid_for_ms != 30000
                {
                    return Err(AccessError::Unavailable);
                }
                let mut leases = self.leases.lock().expect("network leases poisoned");
                if leases.contains_key(&offer.access_id) || leases.len() >= 64 {
                    return Err(AccessError::Busy);
                }
                leases.insert(offer.access_id.clone(), self.ended.child_token());
                if offer.capability != HostNetworkCapability::Ordinary {
                    self.create_desktop(&offer.access_id);
                }
                self.jobs
                    .try_send(Work::Target(offer))
                    .map_err(|_| AccessError::Busy)?;
            }
            Some("access_revoked") => {
                let id = value["access_id"]
                    .as_str()
                    .ok_or(AccessError::Unavailable)?;
                self.desktop
                    .lock()
                    .expect("desktop inbox poisoned")
                    .remove(id);
                self.streams.revoke(id);
                self.targets
                    .lock()
                    .expect("network targets poisoned")
                    .remove(id);
                if let Some(token) = self
                    .leases
                    .lock()
                    .expect("network leases poisoned")
                    .remove(id)
                {
                    token.cancel();
                }
                self.sources
                    .lock()
                    .expect("network sources poisoned")
                    .remove(id);
            }
            _ => return Err(AccessError::Unavailable),
        }
        Ok(())
    }
    pub async fn work(
        self: Arc<Self>,
        work: Work,
        client: CenterClient,
        center: String,
        state: Option<crate::http::HttpState>,
    ) {
        match work {
            Work::Opening(id, source, guard) => {
                let _guard = guard;
                let started = Instant::now();
                let Some(key) = source.permit.user_key() else {
                    return;
                };
                let Ok((_, token, _)) = self.credentials.current_agent_center(key) else {
                    return;
                };
                let mut payload = json!({"access_id":id,"token":token.expose(),"device_id":source.device,"capability":source.capability});
                if let Some(view) = &source.view_id {
                    payload["view_id"] = json!(view);
                }
                let opening = self.request("open", payload);
                let result = tokio::select! {
                    () = source.permit.ended() => return,
                    () = source.cancelled.cancelled() => return,
                    () = source.ready.closed() => return,
                    result = opening => result,
                };
                let outcome = match result {
                    Ok(value) if value["access_id"] == id && source.permit.check().is_ok() => {
                        Ok(())
                    }
                    Err(AccessError::Unauthorized | AccessError::NetworkDenied) => {
                        Err(SourceFailure::Denied)
                    }
                    Err(AccessError::Busy) => Err(SourceFailure::Busy),
                    _ => Err(SourceFailure::Ended),
                };
                source.ready.send_replace(Some(outcome));
                if outcome.is_err() {
                    return;
                }
                self.maintain(&id, &source.permit, &source.cancelled, false, started)
                    .await;
            }
            Work::Proxy(flow, permit, kind) => {
                if let Some(state) = state {
                    match kind {
                        super::streams::Kind::Http => {
                            crate::http::proxy::serve(state, flow, permit).await
                        }
                        super::streams::Kind::Terminal => {
                            crate::user_terminal::bridge::submit(state, flow, permit)
                        }
                    }
                }
            }
            #[cfg(test)]
            Work::Source(id, source, guard, started) => {
                let _guard = guard;
                self.maintain(&id, &source.permit, &source.cancelled, false, started)
                    .await;
            }
            Work::Target(offer) => {
                let _guard = LeaseGuard {
                    session: self.clone(),
                    id: offer.access_id.clone(),
                };
                let cancelled = self
                    .leases
                    .lock()
                    .expect("network leases poisoned")
                    .get(&offer.access_id)
                    .cloned();
                let Some(cancelled) = cancelled else {
                    return;
                };
                let init = self.initialize(
                    &offer,
                    &client,
                    center,
                    state.as_ref().and_then(|state| state.domains.clone()),
                );
                let (permit, checked) = tokio::select! {
                    () = cancelled.cancelled() => return,
                    result = timeout(Duration::from_millis(offer.valid_for_ms), init) => match result { Ok(Ok(permit)) => permit, _ => return },
                };
                self.targets
                    .lock()
                    .expect("network targets poisoned")
                    .insert(offer.access_id.clone(), (permit.clone(), offer.capability));
                let _permit_guard = PermitGuard(permit.clone());
                if offer.capability != HostNetworkCapability::Ordinary {
                    let Some(owner) = state.and_then(|state| state.desktop) else {
                        return;
                    };
                    let Ok(access) = self.desktop_access(
                        offer.access_id.clone(),
                        permit.clone(),
                        cancelled.clone(),
                    ) else {
                        return;
                    };
                    let identity = crate::desktop_media::control::Identity {
                        source: offer.source_connection_id.clone().unwrap_or_default(),
                        user: offer.user_id,
                    };
                    let submitted = if let Some(view) = &offer.view_id {
                        owner.submit_control(view, identity, access)
                    } else {
                        owner.submit_target(access, identity)
                    };
                    if submitted.is_err() {
                        return;
                    }
                }
                self.maintain(&offer.access_id, &permit, &cancelled, true, checked)
                    .await;
            }
        }
    }
    async fn initialize(
        self: &Arc<Self>,
        offer: &Offer,
        client: &CenterClient,
        center_id: String,
        domains: Option<Arc<UserDomains>>,
    ) -> Result<(AccessPermit, Instant), AccessError> {
        let cancelled = self
            .leases
            .lock()
            .expect("network leases poisoned")
            .get(&offer.access_id)
            .cloned()
            .ok_or(AccessError::Unauthorized)?;
        let key = UserKey {
            center_id,
            user_id: offer.user_id,
        };
        let initialized = self
            .credentials
            .network_user(client, key.clone(), cancelled, || {
                self.request("login", json!({"access_id":offer.access_id}))
            })
            .await?;
        // 准入先激活本人用户域，后续业务流复用该 permit，不再借本机原生身份。
        let _capability = offer.capability;
        domains
            .ok_or(AccessError::Unavailable)?
            .ensure(initialized.permit())
            .await
            .map_err(|_| AccessError::Unavailable)?;
        let (_, token, _) = self.credentials.current_agent_center(&key)?;
        let checked = Instant::now();
        let ready = self
            .request(
                "ready",
                json!({"access_id":offer.access_id, "target_token":token.expose()}),
            )
            .await?;
        if ready["access_id"] != offer.access_id || ready["valid_for_ms"] != 45000 {
            return Err(AccessError::Unavailable);
        }
        Ok((initialized.commit()?, checked))
    }
    async fn maintain(
        &self,
        id: &str,
        permit: &AccessPermit,
        cancelled: &CancellationToken,
        target: bool,
        mut checked: Instant,
    ) {
        loop {
            tokio::select! {
                () = self.ended.cancelled() => break,
                () = cancelled.cancelled() => break,
                () = permit.ended() => break,
                () = tokio::time::sleep_until(checked + Duration::from_secs(20)) => {},
            }
            // 睡眠恢复或 owner 调度迟到不能在过期之后复活许可。
            if Instant::now() >= checked + Duration::from_secs(45) {
                break;
            }
            let started = Instant::now();
            let payload = if target {
                let Some(key) = permit.user_key() else {
                    break;
                };
                let Ok((_, token, _)) = self.credentials.current_agent_center(key) else {
                    break;
                };
                json!({"access_id":id,"target_token":token.expose()})
            } else {
                json!({"access_id":id})
            };
            let renewal = self.request(if target { "renew" } else { "renew_source" }, payload);
            let result = tokio::select! {
                () = cancelled.cancelled() => break,
                () = permit.ended() => break,
                result = tokio::time::timeout_at(checked + Duration::from_secs(45), renewal) => result,
            };
            if !matches!(result, Ok(Ok(ref value)) if value["valid_for_ms"] == 45000) {
                break;
            }
            checked = started;
        }
    }
}
struct RequestGuard<'a> {
    session: &'a Session,
    id: &'a str,
}
impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        self.session
            .pending
            .lock()
            .expect("network requests poisoned")
            .remove(self.id);
    }
}
pub(super) struct LeaseGuard {
    pub session: Arc<Session>,
    pub id: String,
}
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.session.close_access(&self.id);
    }
}
struct PermitGuard(AccessPermit);
impl Drop for PermitGuard {
    fn drop(&mut self) {
        self.0.logout();
    }
}
