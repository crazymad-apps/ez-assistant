//! 连接内有界业务流。读写由原连接 owner 驱动，没有独立 socket 或后台任务。
mod frame;
#[cfg(test)]
mod tests;
pub(crate) use frame::Frame;
use frame::{BLOCK, WINDOW};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::Notify,
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub(crate) enum StreamError {
    #[error("network_stream_failed")]
    Ended,
    #[error("network_busy")]
    Busy,
    #[error("network_timeout")]
    Timeout,
    #[error("network_stream_invalid")]
    Invalid,
}
type Key = (String, u64);
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Http,
    Terminal,
}

struct Slot {
    kind: Kind,
    source: bool,
    ready: bool,
    head: bool,
    received_fin: bool,
    sent_fin: bool,
    credit: usize,
    budget: usize,
    credit_pending: usize,
    incoming: VecDeque<Frame>,
    outgoing: VecDeque<Frame>,
    error: Option<StreamError>,
    cancelled: CancellationToken,
    progress: Instant,
}
struct State {
    slots: HashMap<Key, Slot>,
    order: VecDeque<Key>,
    controls: VecDeque<Value>,
    next: u64,
    highest: HashMap<String, u64>,
}
pub(crate) struct Streams {
    state: Mutex<State>,
    changed: Notify,
    pub ended: CancellationToken,
}
pub(crate) struct Flow {
    pub access: String,
    pub id: u64,
    streams: Arc<Streams>,
}
pub(super) struct Outgoing {
    pub message: Message,
    key: Option<Key>,
    fin: bool,
}

impl Streams {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                slots: HashMap::new(),
                order: VecDeque::new(),
                controls: VecDeque::new(),
                next: 0,
                highest: HashMap::new(),
            }),
            changed: Notify::new(),
            ended: CancellationToken::new(),
        })
    }
    pub fn close(&self) {
        self.ended.cancel();
        self.changed.notify_waiters();
    }
    fn control_locked(&self, state: &mut State, value: Value) {
        if state.controls.len() >= 1024 {
            self.ended.cancel();
        } else {
            state.controls.push_back(value);
        }
        self.changed.notify_waiters();
    }
    pub fn open(
        self: &Arc<Self>,
        access: String,
        cancelled: CancellationToken,
        kind: Kind,
    ) -> Result<Flow, StreamError> {
        let mut state = self.state.lock().expect("stream state poisoned");
        state.next = state.next.checked_add(1).ok_or(StreamError::Invalid)?;
        let id = state.next;
        self.insert(&mut state, &access, id, true, cancelled, kind)?;
        self.control_locked(
            &mut state,
            json!({"type":"stream_open","access_id":access,"stream_id":id.to_string(),"kind":kind}),
        );
        Ok(Flow {
            access,
            id,
            streams: self.clone(),
        })
    }
    pub fn accept(
        self: &Arc<Self>,
        access: String,
        id: u64,
        cancelled: CancellationToken,
        kind: Kind,
    ) -> Result<Flow, StreamError> {
        let mut state = self.state.lock().expect("stream state poisoned");
        if id == 0 || state.highest.get(&access).is_some_and(|high| id <= *high) {
            return Err(StreamError::Invalid);
        }
        state.highest.insert(access.clone(), id);
        self.insert(&mut state, &access, id, false, cancelled, kind)?;
        self.control_locked(
            &mut state,
            json!({"type":"stream_ready","access_id":access,"stream_id":id.to_string()}),
        );
        Ok(Flow {
            access,
            id,
            streams: self.clone(),
        })
    }
    fn insert(
        &self,
        state: &mut State,
        access: &str,
        id: u64,
        source: bool,
        cancelled: CancellationToken,
        kind: Kind,
    ) -> Result<(), StreamError> {
        if self.ended.is_cancelled() || cancelled.is_cancelled() {
            return Err(StreamError::Ended);
        }
        if state.slots.len() >= 32 || state.slots.keys().filter(|(a, _)| a == access).count() >= 16
        {
            return Err(StreamError::Busy);
        }
        let key = (access.to_owned(), id);
        if state.slots.contains_key(&key) {
            return Err(StreamError::Invalid);
        }
        state.order.push_back(key.clone());
        state.slots.insert(
            key,
            Slot {
                kind,
                source,
                ready: !source,
                head: false,
                received_fin: false,
                sent_fin: false,
                credit: WINDOW,
                budget: WINDOW,
                credit_pending: 0,
                incoming: VecDeque::new(),
                outgoing: VecDeque::new(),
                error: None,
                cancelled: cancelled.child_token(),
                progress: Instant::now(),
            },
        );
        Ok(())
    }
    pub fn control(&self, value: &Value) -> Result<(), StreamError> {
        let (access, id) = control_key(value)?;
        let mut state = self.state.lock().expect("stream state poisoned");
        let Some(slot) = state.slots.get_mut(&(access, id)) else {
            return Ok(());
        };
        let valid = match value["type"].as_str() {
            Some("stream_ready") if slot.source && !slot.ready => {
                slot.ready = true;
                true
            }
            Some("stream_credit") => {
                if let Some(bytes) = value["bytes"].as_u64().filter(|n| {
                    *n > 0 && *n <= WINDOW as u64 && slot.credit + *n as usize <= WINDOW
                }) {
                    slot.credit += bytes as usize;
                    slot.progress = Instant::now();
                    true
                } else {
                    false
                }
            }
            Some("stream_reset") => {
                slot.error = Some(if value["code"] == "network_busy" {
                    StreamError::Busy
                } else {
                    StreamError::Ended
                });
                slot.cancelled.cancel();
                true
            }
            _ => false,
        };
        if !valid {
            slot.error = Some(StreamError::Invalid);
            slot.cancelled.cancel();
        }
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn receive(&self, bytes: &[u8]) -> Result<(), StreamError> {
        let (access, id, frame) = frame::decode(bytes)?;
        let mut state = self.state.lock().expect("stream state poisoned");
        let Some(slot) = state.slots.get_mut(&(access, id)) else {
            return Ok(());
        };
        if !slot.ready
            || slot.received_fin
            || frame.cost() > slot.budget
            || !match slot.kind {
                Kind::Http => {
                    if slot.head {
                        matches!(frame.kind, 3 | 4)
                    } else {
                        frame.kind == if slot.source { 2 } else { 1 }
                    }
                }
                Kind::Terminal => {
                    if slot.head {
                        matches!(frame.kind, 6..=10)
                    } else {
                        frame.kind == if slot.source { 6 } else { 5 }
                    }
                }
            }
        {
            slot.error = Some(StreamError::Invalid);
            slot.cancelled.cancel();
        } else {
            slot.head = true;
            slot.received_fin = matches!(frame.kind, 4 | 10);
            slot.budget -= frame.cost();
            slot.incoming.push_back(frame);
        }
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn revoke(&self, access: &str) {
        let mut state = self.state.lock().expect("stream state poisoned");
        state.highest.remove(access);
        for ((a, _), slot) in state.slots.iter_mut() {
            if a == access {
                slot.error = Some(StreamError::Ended);
                slot.cancelled.cancel();
            }
        }
        self.changed.notify_waiters();
    }
    pub(super) async fn outgoing(&self) -> Outgoing {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock().expect("stream state poisoned");
                if let Some(value) = state.controls.pop_front() {
                    return Outgoing {
                        message: Message::Text(value.to_string().into()),
                        key: None,
                        fin: false,
                    };
                }
                for _ in 0..state.order.len() {
                    let key = state.order.pop_front().expect("order length checked");
                    state.order.push_back(key.clone());
                    let Some(slot) = state.slots.get_mut(&key) else {
                        continue;
                    };
                    if slot.cancelled.is_cancelled() || slot.error.is_some() {
                        continue;
                    }
                    if slot.credit_pending > 0 {
                        let bytes = std::mem::take(&mut slot.credit_pending);
                        return Outgoing { message: Message::Text(json!({"type":"stream_credit","access_id":key.0,"stream_id":key.1.to_string(),"bytes":bytes}).to_string().into()), key: None, fin: false };
                    }
                    if let Some(frame) = slot.outgoing.pop_front() {
                        match frame::encode(&key.0, key.1, &frame) {
                            Ok(bytes) => {
                                return Outgoing {
                                    message: Message::Binary(bytes.into()),
                                    key: Some(key),
                                    fin: matches!(frame.kind, 4 | 10),
                                };
                            }
                            Err(error) => slot.error = Some(error),
                        }
                    }
                }
            }
            notified.await;
        }
    }
    pub(super) fn written(&self, packet: &Outgoing) {
        if let Some(key) = &packet.key {
            let mut state = self.state.lock().expect("stream state poisoned");
            if let Some(slot) = state.slots.get_mut(key) {
                if packet.fin {
                    slot.sent_fin = true;
                }
                slot.progress = Instant::now();
            }
        }
        self.changed.notify_waiters();
    }
}
pub(crate) fn control_key(value: &Value) -> Result<(String, u64), StreamError> {
    let access = value["access_id"].as_str().ok_or(StreamError::Invalid)?;
    if !super::super::store::valid_id(access) {
        return Err(StreamError::Invalid);
    }
    let raw = value["stream_id"].as_str().ok_or(StreamError::Invalid)?;
    let id = raw.parse::<u64>().map_err(|_| StreamError::Invalid)?;
    if id == 0 || id.to_string() != raw {
        return Err(StreamError::Invalid);
    }
    Ok((access.to_owned(), id))
}
impl Flow {
    pub fn cancelled(&self) -> CancellationToken {
        self.streams
            .state
            .lock()
            .expect("stream state poisoned")
            .slots[&(self.access.clone(), self.id)]
            .cancelled
            .clone()
    }
    pub fn ended(&self) -> CancellationToken {
        self.streams.ended.clone()
    }
    pub async fn ready(&self) -> Result<(), StreamError> {
        timeout(
            Duration::from_secs(10),
            self.wait(|slot| Ok(slot.ready.then_some(()))),
        )
        .await
        .map_err(|_| StreamError::Timeout)?
    }
    async fn wait<T>(
        &self,
        mut take: impl FnMut(&mut Slot) -> Result<Option<T>, StreamError>,
    ) -> Result<T, StreamError> {
        loop {
            let notified = self.streams.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let cancelled = {
                let mut state = self.streams.state.lock().expect("stream state poisoned");
                let slot = state
                    .slots
                    .get_mut(&(self.access.clone(), self.id))
                    .ok_or(StreamError::Ended)?;
                if let Some(error) = slot.error {
                    return Err(error);
                }
                if self.streams.ended.is_cancelled() || slot.cancelled.is_cancelled() {
                    return Err(StreamError::Ended);
                }
                if let Some(value) = take(slot)? {
                    self.streams.changed.notify_waiters();
                    return Ok(value);
                }
                slot.cancelled.clone()
            };
            tokio::select! { () = cancelled.cancelled() => {}, () = self.streams.ended.cancelled() => return Err(StreamError::Ended), () = notified => {} }
        }
    }
    pub async fn send(&self, frame: Frame) -> Result<(), StreamError> {
        frame.validate()?;
        timeout(
            Duration::from_secs(30),
            self.wait(|slot| {
                if !slot.ready || slot.credit < frame.cost() {
                    return Ok(None);
                }
                slot.credit -= frame.cost();
                slot.outgoing.push_back(frame.clone());
                Ok(Some(()))
            }),
        )
        .await
        .map_err(|_| StreamError::Timeout)?
    }
    pub async fn bytes(&self, bytes: &[u8]) -> Result<(), StreamError> {
        for chunk in bytes.chunks(BLOCK) {
            self.send(Frame {
                kind: 3,
                payload: chunk.to_vec(),
            })
            .await?;
        }
        Ok(())
    }
    pub async fn recv(&self) -> Result<Frame, StreamError> {
        self.wait(|slot| {
            let Some(frame) = slot.incoming.pop_front() else {
                return Ok(None);
            };
            slot.budget += frame.cost();
            slot.credit_pending += frame.cost();
            Ok(Some(frame))
        })
        .await
    }
    pub async fn finish(&self) -> Result<(), StreamError> {
        timeout(
            Duration::from_secs(30),
            self.wait(|slot| Ok((slot.sent_fin && slot.received_fin).then_some(()))),
        )
        .await
        .map_err(|_| StreamError::Timeout)?
    }
}
impl Drop for Flow {
    fn drop(&mut self) {
        let mut state = self.streams.state.lock().expect("stream state poisoned");
        let key = (self.access.clone(), self.id);
        if let Some(slot) = state.slots.remove(&key) {
            state.order.retain(|k| k != &key);
            if !slot.sent_fin || !slot.received_fin || slot.error.is_some() {
                self.streams.control_locked(&mut state, json!({"type":"stream_reset","access_id":self.access,"stream_id":self.id.to_string()}));
            }
        }
        self.streams.changed.notify_waiters();
    }
}
