//! 本机两段输入转交；开启控制的中心等待独立轮询，不阻塞视频，也不自动重试副作用。
use super::MediaError;
use crate::access::{AccessError, registration::network::DesktopAccess};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent};

pub(in crate::desktop_media) enum Command {
    Viewed,
    Start(u64),
    Stop,
    Status,
}
type Opening = BoxFuture<'static, Result<DesktopAccess, AccessError>>;
pub(in crate::desktop_media) async fn run(
    view: String,
    open: Box<dyn Fn() -> Opening + Send>,
    local: Arc<dyn DataChannel>,
    remote: Arc<dyn DataChannel>,
    mut commands: mpsc::Receiver<Command>,
) -> Result<(), MediaError> {
    let mut request_id = 0;
    let mut pending_start = false;
    let mut opening: Option<Opening> = None;
    let mut control: Option<DesktopAccess> = None;
    let mut ready: Option<Value> = None;
    let (mut viewed, mut sent, mut remote_open) = (false, false, false);
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased;
            _ = tick.tick() => {
                if viewed && remote_open && !sent { super::send(&remote,json!({"type":"view_ready","view_id":view})).await?; sent = true; }
                if pending_start && viewed && remote_open && control.is_none() && opening.is_none() { pending_start = false; opening = Some(open()); }
                if control.as_ref().is_some_and(|c| c.cancelled.is_cancelled() || c.permit.check().is_err()) {
                    let id = control.take().map(|c| c.id.clone()); ready = None;
                    super::send(&local,json!({"type":"control_stopped","request_id":request_id,"control_id":id,"code":"control_ended"})).await?;
                }
                if let (Some(active), Some(value)) = (&control, &ready)
                    && value["control_id"] == active.id { let stopped = value["type"] == "control_stopped"; let mut value = value.clone(); value["request_id"] = request_id.into(); super::send(&local,value).await?; ready = None; if stopped { control = None; }
                }
            }
            command = commands.recv() => match command {
                Some(Command::Viewed) => viewed = true,
                Some(Command::Start(id)) if id > request_id && viewed && control.is_none() && opening.is_none() => {
                    request_id = id;
                    pending_start = true;
                },
                Some(Command::Start(_) | Command::Status) => { if sent && opening.is_none() && !pending_start { super::send(&remote,json!({"type":"control_status","request_id":request_id})).await?; } },
                Some(Command::Stop) => { pending_start = false; opening = None; control = None; ready = None; super::send(&local,json!({"type":"control_stopped","request_id":request_id,"code":"control_ended"})).await?; },
                None => return Ok(()),
            },
            result = async { opening.as_mut().expect("guarded opening").await }, if opening.is_some() => {
                opening = None;
                match result { Ok(access) => control = Some(access), Err(_) => { ready = None; super::send(&local,json!({"type":"control_stopped","request_id":request_id,"code":"control_denied"})).await?; } }
            }
            event = local.poll() => {
                if let Some(value) = super::message(event)?
                    && let Some(active) = &control
                    && value["view_id"] == view && value["control_id"] == active.id && active.permit.check().is_ok() && !active.cancelled.is_cancelled() {
                            // 满即停止本次控制；停止通过独立中心连接发送，不排在数据通道队尾。
                            if super::send(&remote,value).await.is_err() { control = None; ready = None; super::send(&local,json!({"type":"control_stopped","request_id":request_id,"code":"input_stalled"})).await?; }
                }
            }
            event = remote.poll() => {
                if matches!(event, Some(DataChannelEvent::OnOpen)) { remote_open = true; }
                if let Some(value) = super::message(event)? {
                    match value["type"].as_str() {
                        Some("control_ready") if value["view_id"] == view => ready = Some(value),
                        Some("control_stopped") if control.as_ref().is_some_and(|c| value["control_id"] == c.id || (value["control_id"].is_null() && value["request_id"] == request_id)) => {
                            ready = None; control = None;
                            let mut value = value; value["request_id"] = request_id.into(); super::send(&local,value).await?;
                        },
                        Some("control_stopped") if opening.is_some() => ready = Some(value),
                        Some("input_ack") if control.as_ref().is_some_and(|c| value["control_id"] == c.id) => super::send(&local,value).await?,
                        _ => {},
                    }
                }
            }
        }
    }
}
