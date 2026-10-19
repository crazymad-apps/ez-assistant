//! 登记管理仅转发到冻结的本机 Host；原生证明不进入 WebView。
use super::{ConnectionError, RuntimeTarget, decode_json, failure, http, stale, unavailable};
use assistant_protocol::{HostRegistrationCommand, HostRegistrationStatus};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use tauri::{Manager as _, Resource, ipc::Channel};
use tokio_util::sync::CancellationToken;

#[tauri::command]
pub(crate) async fn manage_host_registration(
    target: RuntimeTarget,
    command: HostRegistrationCommand,
) -> Result<HostRegistrationStatus, ConnectionError> {
    target.ensure_local()?;
    let bootstrap = target.bootstrap().await?;
    let work = async {
        let response = http()?
            .post(format!("{}/host-network/registration", bootstrap.base_url))
            .bearer_auth(&bootstrap.access_token)
            .headers(target.native_headers())
            .json(&command)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            let error: serde_json::Value = decode_json(response).await?;
            return Err(ConnectionError {
                code: "registration_failed",
                message: error
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("登记操作未完成，请查询最新状态。")
                    .to_owned(),
            });
        }
        decode_json(response).await
    };
    let result = tokio::select! {
        biased;
        () = target.cancellation.cancelled() => return Err(stale()),
        result = work => result,
    };
    target.ensure_active()?;
    result
}

struct RegistrationSubscription(CancellationToken);
impl Resource for RegistrationSubscription {
    fn close(self: Arc<Self>) {
        self.0.cancel();
    }
}
impl Drop for RegistrationSubscription {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RegistrationEvent {
    Ready { resource_id: u32 },
    Changed,
}

/// 调用 future 持有 SSE，没有游离任务。页面关闭释放 resource；连接切换立即取消。
/// 有界观察周期也覆盖 WebView 重载而来不及释放句柄的情况，UI 按原入口重新订阅。
#[tauri::command]
pub(crate) async fn watch_host_registration(
    target: RuntimeTarget,
    webview: tauri::Webview,
    events: Channel<RegistrationEvent>,
) -> Result<(), ConnectionError> {
    target.ensure_local()?;
    let cancellation = target.cancellation.child_token();
    let id = webview
        .resources_table()
        .add(RegistrationSubscription(cancellation.clone()));
    let result = async {
        events
            .send(RegistrationEvent::Ready { resource_id: id })
            .map_err(|_| unavailable())?;
        let bootstrap = target.bootstrap().await?;
        let mut response = http()?
            .get(format!("{}/host-network/events", bootstrap.base_url))
            .timeout(Duration::from_secs(30))
            .bearer_auth(&bootstrap.access_token)
            .headers(target.native_headers())
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(failure(
                "registration_events_failed",
                "登记状态通知不可用，请刷新状态。",
            ));
        }
        let mut buffer = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if buffer.len() + chunk.len() > 64 * 1024 {
                return Err(unavailable());
            }
            buffer.extend_from_slice(&chunk);
            while let Some(end) = buffer.windows(2).position(|pair| pair == b"\n\n") {
                let frame = String::from_utf8_lossy(&buffer[..end]);
                if frame
                    .lines()
                    .any(|line| line.trim() == "event: host_network_changed")
                {
                    events
                        .send(RegistrationEvent::Changed)
                        .map_err(|_| unavailable())?;
                }
                buffer.drain(..end + 2);
            }
        }
        Ok(())
    };
    let outcome = tokio::select! {
        biased;
        () = cancellation.cancelled() => Ok(()),
        _ = tokio::time::sleep(Duration::from_secs(25)) => Ok(()),
        result = result => result,
    };
    let _ = webview.resources_table().close(id);
    outcome
}

#[tauri::command]
pub(crate) async fn local_computer_name(target: RuntimeTarget) -> Result<String, ConnectionError> {
    target.ensure_local()?;
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").unwrap_or_default();
    #[cfg(not(windows))]
    let name = {
        let mut command = tokio::process::Command::new("/bin/hostname");
        command.kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(2), command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            }
            _ => String::new(),
        }
    };
    target.ensure_active()?;
    Ok(if name.is_empty() {
        "这台电脑".to_owned()
    } else {
        name.chars().filter(|c| !c.is_control()).take(80).collect()
    })
}
