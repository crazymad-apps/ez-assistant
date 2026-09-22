//! 安装更新后的受控交接。仅适配已发布 0.26.0 的关闭命令，不放宽业务协议准入。

use super::*;
use assistant_protocol::{ClientCompatibility, MIN_COMPATIBLE_VERSION_HEADER, RuntimeLifecycle};

#[cfg(test)]
mod tests;

impl RuntimeBootstrapCoordinator {
    /// 只核对随包来源和版本；检测阶段不停止 Host，也不接触用户数据。
    pub(super) async fn upgrade_source(
        &self,
        original: &RuntimeDiscovery,
        capabilities: &RuntimeHostCapabilities,
    ) -> Result<(PathBuf, String), RuntimeBootstrapError> {
        let source = self
            .runtime_executable
            .canonicalize()
            .map_err(|_| source_unavailable())?;
        if capabilities.runtime_version != "0.26.0"
            || capabilities.min_compatible_version != "0.26.0"
            || capabilities.mode != assistant_protocol::HostMode::Personal
            || assistant_protocol::parse_software_version(assistant_protocol::SOFTWARE_VERSION)
                <= assistant_protocol::parse_software_version(&capabilities.runtime_version)
            || original.executable_path.as_ref() != Some(&source)
            || original
                .executable_sha256
                .as_ref()
                .is_none_or(|hash| hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(source_unavailable());
        }
        let path = source.clone();
        let digest = tokio::task::spawn_blocking(move || crate::runtime_source::digest(&path))
            .await
            .map_err(|_| source_unavailable())?
            .map_err(|_| source_unavailable())?;
        crate::runtime_source::verify(&source, &digest, &ClientCompatibility::current())
            .await
            .map_err(|_| source_unavailable())?;
        Ok((source, digest))
    }

    /// 仅由用户明确的“更新并重启”调用。串行固定实例的关闭与启动；失败不补发、不强杀。
    /// 数据备份和迁移归新版 Host，桌面只等待发现与业务准入，不能回滚或修复数据库。
    pub(crate) async fn upgrade(&self) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        let _operation = self.lifecycle_gate.lock().await;
        // 连续点击或另一入口已经完成更新时复用新实例，不再重启。
        if let Ok(bootstrap) = self.discover(false).await {
            return Ok(bootstrap);
        }
        let (previous, original) = self.inspect_target(false).await?;
        let (source, digest) = self
            .upgrade_source(&original, &previous.capabilities)
            .await?;
        let current = read_discovery(&self.runtime_home)?;
        if current.instance_id != original.instance_id
            || current.pid != original.pid
            || current.access_token != original.access_token
            || current.address != original.address
        {
            return Err(instance_changed());
        }
        self.stop_for_upgrade(&previous).await?;
        self.wait_for_instance_to_stop(&original).await?;
        crate::runtime_source::verify(&source, &digest, &ClientCompatibility::current())
            .await
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStartFailed,
                    "旧 Runtime 已停止，但安装文件发生变化。请重新打开已安装的应用完成更新。",
                )
            })?;
        self.launch_from(&source).await?;
        let bootstrap = self.wait_for_start().await?;
        let current = read_discovery(&self.runtime_home)?;
        if current.instance_id == original.instance_id
            || current.instance_id != bootstrap.instance_id
            || current.executable_path.as_ref() != Some(&source)
            || current.executable_sha256.as_ref() != Some(&digest)
        {
            return Err(instance_changed());
        }
        Ok(bootstrap)
    }

    /// 0.26.0 的关闭 wire 契约经过独立验证。真实软件版本不变，最低版本仅对本窄命令为 0.26.0；
    /// 不复用本通道读取业务数据，也不按服务端声明动态降低版本下限。
    async fn stop_for_upgrade(
        &self,
        previous: &RuntimeBootstrap,
    ) -> Result<(), RuntimeBootstrapError> {
        #[derive(Deserialize)]
        struct Response {
            result: Scope,
        }
        #[derive(Deserialize)]
        #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
        enum Scope {
            Runtime(Result),
        }
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "payload", rename_all = "snake_case")]
        enum Result {
            ShutdownRuntime { lifecycle: RuntimeLifecycle },
        }

        let mut headers = crate::runtime_compatibility::headers();
        headers.insert(
            MIN_COMPATIBLE_VERSION_HEADER,
            reqwest::header::HeaderValue::from_static("0.26.0"),
        );
        let response = self.http.post(format!("{}/commands", previous.base_url))
            .timeout(CONTROL_COMMAND_TIMEOUT)
            .bearer_auth(&previous.access_token)
            .headers(headers)
            .json(&serde_json::json!({
                "request_id": "desktop-upgrade-runtime",
                "command": { "scope": "runtime", "payload": { "type": "shutdown_runtime", "payload": {} } }
            }))
            .send().await.map_err(runtime_stop_failed)?
            .error_for_status().map_err(runtime_stop_failed)?
            .json::<Response>().await.map_err(runtime_stop_failed)?;
        let Scope::Runtime(Result::ShutdownRuntime { lifecycle }) = response.result;
        if !matches!(
            lifecycle,
            RuntimeLifecycle::ShuttingDown | RuntimeLifecycle::Stopped
        ) {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeStopFailed,
                "旧 Runtime 尚未接受更新重启，请稍后重试。",
            ));
        }
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn upgrade_runtime(
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    lifecycle: State<'_, DesktopLifecycleCoordinator>,
) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
    lifecycle.update_runtime_state(NativeRuntimeState::Restarting);
    let result = coordinator.upgrade().await;
    lifecycle.update_runtime_state(if result.is_ok() {
        NativeRuntimeState::Connected
    } else {
        NativeRuntimeState::Disconnected
    });
    result
}
