//! 本机登录配置的管理投影与重启前置核对；配置权威仍在 Host。

use assistant_protocol::{
    DesktopUpdateContext, HostAccessCommand, HostAccessStatus, HostMode, RuntimeErrorCode,
    RuntimeErrorInfo,
};

use super::*;

/// 仅原生 IPC 使用的已保存配置与运行中身份组合，不包含管理凭据。
#[derive(Debug, Serialize)]
pub(crate) struct LocalHostStatus {
    #[serde(flatten)]
    status: HostAccessStatus,
    instance_id: String,
    current_mode: HostMode,
    current_center_url: Option<String>,
}

/// 用户已确认的保存事实；只复用 Host 的实例与配置修订，不创建新版本计数。
#[derive(Debug, Deserialize)]
pub(crate) struct ExpectedHostConfiguration {
    instance_id: String,
    revision: Option<String>,
}

impl ExpectedHostConfiguration {
    pub(super) fn verify_instance(
        &self,
        bootstrap: &RuntimeBootstrap,
    ) -> Result<(), RuntimeBootstrapError> {
        verify_instance(bootstrap, Some(&self.instance_id))
    }
}

impl RuntimeBootstrapCoordinator {
    /// 先取 updater permit 再取 lifecycle gate；写操作不因超时而自动补发。
    /// expected 存在时只发现运行中实例，拒绝为过期表单启动替代进程。
    pub(crate) async fn manage_host(
        &self,
        updater: &crate::desktop_update::DesktopUpdater,
        command: HostAccessCommand,
        expected_instance_id: Option<&str>,
    ) -> Result<LocalHostStatus, RuntimeBootstrapError> {
        let _permit = if matches!(command, HostAccessCommand::GetStatus) {
            updater.lifecycle_permit().await
        } else {
            updater.configuration_permit().await
        }
        .map_err(|_| {
            bootstrap_error(
                RuntimeBootstrapErrorCode::DesktopUpdateBusy,
                "更新正在进行，请完成后重试。",
            )
        })?;
        let _operation = self.lifecycle_gate.lock().await;
        let bootstrap = if expected_instance_id.is_some() {
            self.discover(false).await?
        } else {
            self.bootstrap_under_gate().await?
        };
        verify_instance(&bootstrap, expected_instance_id)?;
        // 在写入前取运行中身份，避免保存后读取失败被误判为未保存。
        let context = self.current_login_context(&bootstrap).await?;
        self.verify_management_target(&bootstrap)?;
        let status = self.host_access_command(&bootstrap, &command).await?;
        // 即使请求已成功，实例变化也只能按结果待核对反馈；不能改投新实例。
        self.verify_management_target(&bootstrap)
            .map_err(|_| configuration_unknown())?;
        Ok(LocalHostStatus {
            status,
            instance_id: bootstrap.instance_id,
            current_mode: bootstrap.capabilities.mode,
            current_center_url: context.center_url,
        })
    }

    /// 调用方持有 lifecycle gate，停止前回读原实例 revision；核对失败不发送 Shutdown。
    pub(super) async fn verify_saved_configuration(
        &self,
        bootstrap: &RuntimeBootstrap,
        expected: &ExpectedHostConfiguration,
    ) -> Result<(), RuntimeBootstrapError> {
        expected.verify_instance(bootstrap)?;
        let status = self
            .host_access_command(bootstrap, &HostAccessCommand::GetStatus)
            .await?;
        if status.revision != expected.revision {
            return Err(configuration_changed());
        }
        self.verify_management_target(bootstrap)
    }

    fn verify_management_target(
        &self,
        bootstrap: &RuntimeBootstrap,
    ) -> Result<(), RuntimeBootstrapError> {
        let discovery = read_discovery(&self.runtime_home)?;
        if discovery.instance_id != bootstrap.instance_id
            || discovery.address != bootstrap.base_url
            || discovery.access_token != bootstrap.access_token
        {
            return Err(configuration_changed());
        }
        Ok(())
    }

    async fn current_login_context(
        &self,
        bootstrap: &RuntimeBootstrap,
    ) -> Result<DesktopUpdateContext, RuntimeBootstrapError> {
        let response = self
            .http
            .get(format!("{}/desktop-update-context", bootstrap.base_url))
            .headers(crate::runtime_compatibility::headers())
            .send()
            .await
            .map_err(runtime_unavailable)?;
        if !response.status().is_success() {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::ComponentMismatch,
                "无法读取运行中的登录配置，请核对 Host 版本后重试。",
            ));
        }
        let context: DesktopUpdateContext = read_management_json(response).await?;
        if (bootstrap.capabilities.mode == HostMode::Enterprise) != context.center_url.is_some() {
            return Err(configuration_changed());
        }
        Ok(context)
    }

    /// 只向已发现的本机 Host 提交一次现有命令；成功后策略发布失败仍可能已经落盘。
    /// 传输、格式或未知错误统一保留“结果待核对”，不从 HTTP 非 2xx 推断未保存。
    async fn host_access_command(
        &self,
        bootstrap: &RuntimeBootstrap,
        command: &HostAccessCommand,
    ) -> Result<HostAccessStatus, RuntimeBootstrapError> {
        let response = self
            .http
            .post(format!("{}/commands", bootstrap.base_url))
            .timeout(Duration::from_secs(15))
            .bearer_auth(&bootstrap.access_token)
            .headers(crate::runtime_compatibility::headers())
            .json(&serde_json::json!({
                "request_id": "desktop-host-management",
                "command": {"scope": "host_access", "payload": command}
            }))
            .send()
            .await
            .map_err(|_| configuration_unknown())?;
        decode_management_response(response).await
    }
}

fn verify_instance(
    bootstrap: &RuntimeBootstrap,
    expected: Option<&str>,
) -> Result<(), RuntimeBootstrapError> {
    if expected.is_some_and(|id| id != bootstrap.instance_id) {
        return Err(configuration_changed());
    }
    Ok(())
}

fn configuration_changed() -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::ConfigurationConflict,
        "本机实例或配置已变化，请重新读取并核对。",
    )
}

fn configuration_unknown() -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::ConfigurationUnavailable,
        "无法确认 Host 配置结果，请重新读取核对，不要重复提交。",
    )
}

async fn decode_management_response(
    response: reqwest::Response,
) -> Result<HostAccessStatus, RuntimeBootstrapError> {
    #[derive(Deserialize)]
    struct Success {
        result: ResultScope,
    }
    #[derive(Deserialize)]
    #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
    enum ResultScope {
        HostAccess(HostAccessStatus),
    }
    #[derive(Deserialize)]
    struct Failure {
        error: RuntimeErrorInfo,
    }
    if response.status().is_success() {
        let Success {
            result: ResultScope::HostAccess(status),
        } = read_management_json(response).await?;
        return Ok(status);
    }
    let Failure { error } = read_management_json(response).await?;
    let code = match error.code {
        RuntimeErrorCode::ConfigurationConflict => RuntimeBootstrapErrorCode::ConfigurationConflict,
        RuntimeErrorCode::InvalidRequest => RuntimeBootstrapErrorCode::InvalidRequest,
        RuntimeErrorCode::OperationNotAllowed => RuntimeBootstrapErrorCode::OperationNotAllowed,
        _ => return Err(configuration_unknown()),
    };
    // 仅信任已鉴权本机 Host 的规范脱敏消息；未知响应正文不会进入 UI。
    Err(RuntimeBootstrapError {
        code,
        message: error.message,
    })
}

async fn read_management_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, RuntimeBootstrapError> {
    const MAX_BYTES: usize = 64 * 1024;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| configuration_unknown())?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err(configuration_unknown());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| configuration_unknown())
}

#[cfg(test)]
mod tests;
