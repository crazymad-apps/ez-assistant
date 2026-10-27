//! 手动覆盖当前安装后的显式换代；停止复用原生身份核对，业务准入保持不变。

use super::*;
use assistant_protocol::ClientCompatibility;

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
        let previous = ClientCompatibility {
            version: capabilities.runtime_version.clone(),
            min_compatible_version: capabilities.min_compatible_version.clone(),
        };
        if !previous.is_valid()
            || assistant_protocol::parse_software_version(assistant_protocol::SOFTWARE_VERSION)
                < assistant_protocol::parse_software_version(&capabilities.runtime_version)
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
        if original.executable_sha256.as_ref() == Some(&digest) {
            return Err(source_unavailable());
        }
        crate::runtime_source::verify(&source, &digest, &ClientCompatibility::current())
            .await
            .map_err(|_| source_unavailable())?;
        Ok((source, digest))
    }

    /// 仅由用户明确的“更新并重启”调用。gate 内核对已替换来源，原生停止后启动当前随包文件。
    /// 数据备份和迁移归新版 Host，桌面只等待发现与业务准入，不能回滚或修复数据库。
    pub(crate) async fn upgrade(&self) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        let operation = self.lifecycle_gate.clone().lock_owned().await;
        // 连续点击或另一入口已完成换代时，不再重启；discover 会先检测待换代来源。
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
        let (_operation, _) = self.stop_native(operation, Some(original.clone())).await?;
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
}

#[tauri::command]
pub(crate) async fn upgrade_runtime(
    updater: State<'_, crate::desktop_update::DesktopUpdater>,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    lifecycle: State<'_, DesktopLifecycleCoordinator>,
) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
    let _permit = updater.lifecycle_permit().await.map_err(|_| {
        bootstrap_error(
            RuntimeBootstrapErrorCode::RuntimeUnavailable,
            "正在安装更新，请稍候。",
        )
    })?;
    lifecycle.update_runtime_state(NativeRuntimeState::Restarting);
    let result = coordinator.upgrade().await;
    lifecycle.update_runtime_state(if result.is_ok() {
        NativeRuntimeState::Connected
    } else {
        NativeRuntimeState::Disconnected
    });
    result
}
