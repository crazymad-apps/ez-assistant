//! 单个查看的输入 owner；整机 semaphore 仅在执行器及释放回收后归还。
use super::{MediaError, native::NativeInput};
use crate::{access::registration::network::DesktopAccess, desktop_media::service::DesktopViews};
use assistant_protocol::{DesktopInput, DesktopInputFrame};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc},
    time::Instant,
};
use webrtc::data_channel::DataChannel;

struct Active {
    access: DesktopAccess,
    native: NativeInput,
    _slot: OwnedSemaphorePermit,
    heartbeat: Instant,
}
#[derive(Default)]
pub(in crate::desktop_media) struct State {
    active: Option<Active>,
}
impl State {
    pub(in crate::desktop_media) async fn stop(&mut self) {
        if let Some(active) = self.active.take() {
            active.native.stop().await;
            drop(active.access);
        }
    }
}
pub(in crate::desktop_media) async fn serve(
    state: &mut State,
    mut grants: mpsc::Receiver<DesktopAccess>,
    data: Arc<dyn DataChannel>,
    owner: Arc<DesktopViews>,
    view: &str,
    display: &str,
    binding: &str,
) -> Result<(), MediaError> {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut viewed = false;
    loop {
        tokio::select! { biased;
            _ = tick.tick() => {
                if state.active.as_ref().is_some_and(|a| a.access.cancelled.is_cancelled() || a.access.permit.check().is_err() || a.heartbeat.elapsed() > Duration::from_secs(1)) {
                    let id = state.active.as_ref().map(|a| a.access.id.clone());
                    state.stop().await;
                    super::send(&data,json!({"type":"control_stopped","control_id":id,"code":"control_ended"})).await?;
                }
            }
            grant = grants.recv() => {
                let Some(access) = grant else { return Err(MediaError::Ended); };
                let id = access.id.clone();
                let slot = owner.controls.exclusive.clone().try_acquire_owned();
                let code = if !viewed || access.cancelled.is_cancelled() || access.permit.check().is_err() { Some("control_denied") }
                    else if state.active.is_some() || slot.is_err() { Some("control_busy") } else { None };
                if let Some(code) = code { super::send(&data,json!({"type":"control_stopped","control_id":id,"code":code})).await?; continue; }
                let slot = slot.map_err(|_| MediaError::Ended)?;
                match NativeInput::start(display, binding).await {
                    Ok(native) => {
                        state.active = Some(Active { access, native, _slot: slot, heartbeat: Instant::now() });
                        if state.active.as_ref().is_some_and(|a| a.access.cancelled.is_cancelled() || a.access.permit.check().is_err()) { state.stop().await; continue; }
                        super::send(&data,json!({"type":"control_ready","view_id":view,"control_id":id,"display_id":display})).await?;
                    }
                    Err(code) => { super::send(&data,json!({"type":"control_stopped","control_id":id,"code":code})).await?; }
                }
            }
            event = data.poll() => {
                let Some(value) = super::message(event)? else { continue; };
                if value["type"] == "view_ready" && value["view_id"] == view { viewed = true; continue; }
                if value["type"] == "control_status" {
                    let mut response = match &state.active { Some(a) => json!({"type":"control_ready","view_id":view,"control_id":a.access.id,"display_id":display}), None => json!({"type":"control_stopped","code":"control_ended"}) };
                    response["request_id"] = value["request_id"].clone(); super::send(&data,response).await?; continue;
                }
                let Some(active) = &mut state.active else { continue; };
                let frame = serde_json::from_value::<DesktopInputFrame>(value);
                let mut error = None;
                match frame {
                    Ok(frame) if frame.control_id == active.access.id && frame.view_id == view && frame.display_id == display => {
                        if active.access.cancelled.is_cancelled() || active.access.permit.check().is_err() || active.heartbeat.elapsed() > Duration::from_secs(1) { error = Some("control_ended"); }
                        else {
                            if matches!(frame.input, DesktopInput::Heartbeat) { active.heartbeat = Instant::now(); }
                            match active.native.command(frame.sequence, &frame.input).await {
                                Ok(()) => super::send(&data,json!({"type":"input_ack","control_id":active.access.id,"sequence":frame.sequence})).await?,
                                Err(code) => error = Some(code),
                            }
                        }
                    }
                    Ok(_) => continue, // 旧绑定永不复活，也不影响新控制。
                    Err(_) => error = Some("invalid_input"),
                }
                if let Some(code) = error {
                    let id = active.access.id.clone();
                    state.stop().await;
                    super::send(&data,json!({"type":"control_stopped","control_id":id,"code":code})).await?;
                }
            }
        }
    }
}
