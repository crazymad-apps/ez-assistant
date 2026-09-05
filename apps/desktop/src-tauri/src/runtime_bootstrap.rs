//! 受信任桌面进程中的 Runtime discovery、启动与 bootstrap。

use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

use assistant_protocol::{
    GetSessionViewRequest, GetWorkspaceRequest, RuntimeCommand, RuntimeCommandResult,
    RuntimeHostCapabilities, RuntimeHostFeature, RuntimeHostHealth, RuntimeHostHealthStatus,
    SessionId, ShutdownRuntimeRequest, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use thiserror::Error;
use url::Url;

use crate::desktop_lifecycle::{DesktopLifecycleCoordinator, NativeRuntimeState};

const DISCOVERY_RELATIVE_PATH: &str = "run/runtime.json";
const MAX_DISCOVERY_BYTES: u64 = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);
const POLL_INTERVAL: Duration = Duration::from_millis(120);
const REQUIRED_FEATURES: &[RuntimeHostFeature] = &[
    RuntimeHostFeature::EventEnvelopes,
    RuntimeHostFeature::ApplicationSnapshot,
    RuntimeHostFeature::SessionView,
    RuntimeHostFeature::ChildTaskView,
    RuntimeHostFeature::SessionManagement,
    RuntimeHostFeature::SessionResourceFiles,
];

#[derive(Clone)]
pub(crate) struct RuntimeBootstrapCoordinator {
    runtime_home: PathBuf,
    runtime_executable: PathBuf,
    http: reqwest::Client,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeDiscovery {
    address: String,
    instance_id: String,
    access_token: String,
    pid: u32,
}

/// 只在 invoke 返回值和 RuntimeClient 私有闭包之间短暂存在的连接凭据。
#[derive(Clone, Debug, Serialize)]
pub(crate) struct RuntimeBootstrap {
    pub(crate) base_url: String,
    instance_id: String,
    pub(crate) access_token: String,
    pub(crate) capabilities: RuntimeHostCapabilities,
    started_runtime: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeBootstrapErrorCode {
    RuntimeHomeUnavailable,
    RuntimeExecutableUnavailable,
    DiscoveryInvalid,
    RuntimeStartFailed,
    RuntimeUnavailable,
    ComponentMismatch,
    RuntimeStopFailed,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub(crate) struct RuntimeBootstrapError {
    code: RuntimeBootstrapErrorCode,
    message: String,
}

impl Serialize for RuntimeBootstrapError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct SafeError<'a> {
            code: RuntimeBootstrapErrorCode,
            message: &'a str,
        }

        SafeError {
            code: self.code,
            message: &self.message,
        }
        .serialize(serializer)
    }
}

impl RuntimeBootstrapCoordinator {
    pub(crate) fn for_application() -> Self {
        let runtime_home = dirs::home_dir()
            .map(|home| home.join(".ez-assistant"))
            .unwrap_or_default();
        let runtime_executable = resolve_runtime_executable().unwrap_or_default();
        Self {
            runtime_home,
            runtime_executable,
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    #[cfg(test)]
    fn new(
        runtime_home: PathBuf,
        runtime_executable: PathBuf,
    ) -> Result<Self, RuntimeBootstrapError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeUnavailable,
                    "无法初始化本地 Runtime 连接。",
                )
            })?;
        Ok(Self {
            runtime_home,
            runtime_executable,
            http,
        })
    }

    pub(crate) async fn bootstrap(&self) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        if self.runtime_home.as_os_str().is_empty() {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeHomeUnavailable,
                "无法确定 Runtime Home。",
            ));
        }
        match self.discover(false).await {
            Ok(bootstrap) => return Ok(bootstrap),
            Err(error) if matches!(error.code, RuntimeBootstrapErrorCode::ComponentMismatch) => {
                return Err(error);
            }
            Err(_) => {}
        }

        self.launch()?;
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Ok(bootstrap) = self.discover(true).await {
                return Ok(bootstrap);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeUnavailable,
                    "Runtime 未能在限定时间内启动，请检查配置后重试。",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), RuntimeBootstrapError> {
        let bootstrap = self.discover(false).await?;
        self.send_runtime_command(
            "desktop-stop-runtime",
            RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default()),
        )
        .await?;
        self.wait_for_instance_to_stop(&bootstrap.instance_id).await
    }

    pub(crate) async fn restart(&self) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        let previous = self.discover(false).await?;
        self.send_runtime_command(
            "desktop-restart-runtime",
            RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default()),
        )
        .await?;
        self.wait_for_instance_to_stop(&previous.instance_id)
            .await?;
        self.launch()?;

        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Ok(bootstrap) = self.discover(true).await
                && bootstrap.instance_id != previous.instance_id
            {
                return Ok(bootstrap);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeUnavailable,
                    "Runtime 重启后未能在限定时间内就绪。",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    fn runtime_home(&self) -> Result<&Path, RuntimeBootstrapError> {
        if self.runtime_home.as_os_str().is_empty() || !self.runtime_home.is_dir() {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeHomeUnavailable,
                "Runtime Home 当前不可用。",
            ));
        }
        Ok(&self.runtime_home)
    }

    pub(crate) async fn workspace_directory(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<String, RuntimeBootstrapError> {
        #[derive(Serialize)]
        struct CommandRequest {
            request_id: String,
            command: CommandScope,
        }
        #[derive(Serialize)]
        #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
        enum CommandScope {
            Runtime(RuntimeCommand),
        }
        #[derive(Deserialize)]
        struct CommandResponse {
            result: ResultScope,
        }
        #[derive(Deserialize)]
        #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
        enum ResultScope {
            Runtime(RuntimeCommandResult),
        }

        let bootstrap = self.bootstrap().await?;
        let response = self
            .http
            .post(format!("{}/commands", bootstrap.base_url))
            .bearer_auth(&bootstrap.access_token)
            .json(&CommandRequest {
                request_id: "desktop-open-workspace".to_owned(),
                command: CommandScope::Runtime(RuntimeCommand::GetWorkspace(GetWorkspaceRequest {
                    workspace_id,
                })),
            })
            .send()
            .await
            .map_err(runtime_unavailable)?
            .error_for_status()
            .map_err(runtime_unavailable)?
            .json::<CommandResponse>()
            .await
            .map_err(runtime_unavailable)?;
        match response.result {
            ResultScope::Runtime(RuntimeCommandResult::GetWorkspace(result)) => {
                Ok(result.workspace.user_directory)
            }
            _ => Err(bootstrap_error(
                RuntimeBootstrapErrorCode::ComponentMismatch,
                "Runtime 返回了不匹配的 Workspace 结果。",
            )),
        }
    }

    pub(crate) async fn session_workspace_directory(
        &self,
        session_id: SessionId,
        directory_index: usize,
    ) -> Result<String, RuntimeBootstrapError> {
        let result = self
            .send_runtime_command(
                "desktop-open-session-workspace-directory",
                RuntimeCommand::GetSessionView(GetSessionViewRequest { session_id }),
            )
            .await?;
        let RuntimeCommandResult::GetSessionView(result) = result else {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::ComponentMismatch,
                "Runtime 返回了不匹配的 Session View 结果。",
            ));
        };
        let workspace = result.snapshot.value.workspace.ok_or_else(|| {
            bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeUnavailable,
                "该会话未绑定工作空间。",
            )
        })?;
        if directory_index == 0 {
            return Ok(workspace.primary_directory);
        }
        workspace
            .additional_directories
            .get(directory_index - 1)
            .cloned()
            .ok_or_else(|| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeUnavailable,
                    "该会话的工作目录不存在。",
                )
            })
    }

    async fn send_runtime_command(
        &self,
        request_id: &str,
        command: RuntimeCommand,
    ) -> Result<RuntimeCommandResult, RuntimeBootstrapError> {
        #[derive(Serialize)]
        struct CommandRequest {
            request_id: String,
            command: CommandScope,
        }
        #[derive(Serialize)]
        #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
        enum CommandScope {
            Runtime(RuntimeCommand),
        }
        #[derive(Deserialize)]
        struct CommandResponse {
            result: ResultScope,
        }
        #[derive(Deserialize)]
        #[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
        enum ResultScope {
            Runtime(Box<RuntimeCommandResult>),
        }

        let bootstrap = self.discover(false).await?;
        let response = self
            .http
            .post(format!("{}/commands", bootstrap.base_url))
            .timeout(CONTROL_COMMAND_TIMEOUT)
            .bearer_auth(&bootstrap.access_token)
            .json(&CommandRequest {
                request_id: request_id.to_owned(),
                command: CommandScope::Runtime(command),
            })
            .send()
            .await
            .map_err(runtime_stop_failed)?
            .error_for_status()
            .map_err(runtime_stop_failed)?
            .json::<CommandResponse>()
            .await
            .map_err(runtime_stop_failed)?;
        match response.result {
            ResultScope::Runtime(result) => Ok(*result),
        }
    }

    async fn wait_for_instance_to_stop(
        &self,
        instance_id: &str,
    ) -> Result<(), RuntimeBootstrapError> {
        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match read_discovery(&self.runtime_home) {
                Ok(discovery)
                    if discovery.instance_id == instance_id && process_is_alive(discovery.pid) => {}
                _ => return Ok(()),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStopFailed,
                    "Runtime 未能在限定时间内受控停止。",
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn discover(
        &self,
        started_runtime: bool,
    ) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        let discovery = read_discovery(&self.runtime_home)?;
        validate_discovery(&discovery)?;
        if !process_is_alive(discovery.pid) {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::DiscoveryInvalid,
                "Runtime discovery 指向的进程已失效。",
            ));
        }
        let capabilities = self.verify_endpoint(&discovery).await?;
        let missing = REQUIRED_FEATURES
            .iter()
            .find(|feature| !capabilities.features.contains(feature));
        if missing.is_some() || !capabilities.sse {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::ComponentMismatch,
                "随包 Runtime 缺少当前桌面端所需能力。",
            ));
        }
        Ok(RuntimeBootstrap {
            base_url: discovery.address,
            instance_id: discovery.instance_id,
            access_token: discovery.access_token,
            capabilities,
            started_runtime,
        })
    }

    async fn verify_endpoint(
        &self,
        discovery: &RuntimeDiscovery,
    ) -> Result<RuntimeHostCapabilities, RuntimeBootstrapError> {
        let health = self
            .http
            .get(format!("{}/health", discovery.address))
            .bearer_auth(&discovery.access_token)
            .send()
            .await
            .map_err(runtime_unavailable)?
            .error_for_status()
            .map_err(runtime_unavailable)?
            .json::<RuntimeHostHealth>()
            .await
            .map_err(runtime_unavailable)?;
        if health.status != RuntimeHostHealthStatus::Ready {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeUnavailable,
                "Runtime 尚未就绪。",
            ));
        }
        self.http
            .get(format!("{}/capabilities", discovery.address))
            .bearer_auth(&discovery.access_token)
            .send()
            .await
            .map_err(runtime_unavailable)?
            .error_for_status()
            .map_err(runtime_unavailable)?
            .json::<RuntimeHostCapabilities>()
            .await
            .map_err(runtime_unavailable)
    }

    fn launch(&self) -> Result<(), RuntimeBootstrapError> {
        if !self.runtime_executable.is_file() {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable,
                "未找到随包 Runtime 组件。",
            ));
        }
        let status = Command::new(&self.runtime_executable)
            .arg("launch")
            .arg("--runtime-home")
            .arg(&self.runtime_home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStartFailed,
                    "无法启动随包 Runtime 组件。",
                )
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeStartFailed,
                "随包 Runtime 组件启动失败。",
            ))
        }
    }
}

#[tauri::command]
pub(crate) async fn bootstrap_runtime(
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    lifecycle: State<'_, DesktopLifecycleCoordinator>,
) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
    lifecycle.update_runtime_state(NativeRuntimeState::Connecting);
    let result = coordinator.bootstrap().await;
    lifecycle.update_runtime_state(if result.is_ok() {
        NativeRuntimeState::Connected
    } else {
        NativeRuntimeState::Disconnected
    });
    result
}

#[tauri::command]
pub(crate) async fn stop_runtime(
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    lifecycle: State<'_, DesktopLifecycleCoordinator>,
) -> Result<(), RuntimeBootstrapError> {
    lifecycle.update_runtime_state(NativeRuntimeState::Stopping);
    let result = coordinator.shutdown().await;
    lifecycle.update_runtime_state(if result.is_ok() {
        NativeRuntimeState::Stopped
    } else {
        NativeRuntimeState::Disconnected
    });
    result
}

#[tauri::command]
pub(crate) async fn restart_runtime(
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
    lifecycle: State<'_, DesktopLifecycleCoordinator>,
) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
    lifecycle.update_runtime_state(NativeRuntimeState::Restarting);
    let result = coordinator.restart().await;
    lifecycle.update_runtime_state(if result.is_ok() {
        NativeRuntimeState::Connected
    } else {
        NativeRuntimeState::Disconnected
    });
    result
}

#[tauri::command]
pub(crate) fn open_runtime_home(
    app: tauri::AppHandle,
    coordinator: State<'_, RuntimeBootstrapCoordinator>,
) -> Result<(), RuntimeBootstrapError> {
    let runtime_home = coordinator.runtime_home()?;
    let runtime_home = runtime_home.to_str().ok_or_else(|| {
        bootstrap_error(
            RuntimeBootstrapErrorCode::RuntimeHomeUnavailable,
            "Runtime Home 路径无法交给系统文件管理器处理。",
        )
    })?;
    app.opener()
        .open_path(runtime_home, None::<&str>)
        .map_err(|_| {
            bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeHomeUnavailable,
                "无法使用系统文件管理器打开 Runtime Home。",
            )
        })
}

fn resolve_runtime_executable() -> Result<PathBuf, RuntimeBootstrapError> {
    if let Some(path) = std::env::var_os("EZ_ASSISTANT_RUNTIME_BIN") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let current = std::env::current_exe().map_err(|_| {
        bootstrap_error(
            RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable,
            "无法定位桌面应用程序。",
        )
    })?;
    let directory = current.parent().ok_or_else(|| {
        bootstrap_error(
            RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable,
            "无法定位随包 Runtime 目录。",
        )
    })?;
    Ok(directory.join(format!(
        "ez-assistant-runtime{}",
        std::env::consts::EXE_SUFFIX
    )))
}

fn read_discovery(runtime_home: &Path) -> Result<RuntimeDiscovery, RuntimeBootstrapError> {
    let path = runtime_home.join(DISCOVERY_RELATIVE_PATH);
    let metadata = fs::symlink_metadata(&path).map_err(|_| invalid_discovery())?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_DISCOVERY_BYTES {
        return Err(invalid_discovery());
    }
    #[cfg(unix)]
    {
        if metadata.mode() & 0o077 != 0 {
            return Err(invalid_discovery());
        }
        let parent = path.parent().ok_or_else(invalid_discovery)?;
        let parent_metadata = fs::metadata(parent).map_err(|_| invalid_discovery())?;
        if metadata.uid() != parent_metadata.uid() {
            return Err(invalid_discovery());
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&path).map_err(|_| invalid_discovery())?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_DISCOVERY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_discovery())?;
    if bytes.len() as u64 > MAX_DISCOVERY_BYTES {
        return Err(invalid_discovery());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid_discovery())
}

fn validate_discovery(discovery: &RuntimeDiscovery) -> Result<(), RuntimeBootstrapError> {
    if discovery.instance_id.trim().is_empty()
        || discovery.access_token.len() < 32
        || discovery.pid == 0
    {
        return Err(invalid_discovery());
    }
    let address = Url::parse(&discovery.address).map_err(|_| invalid_discovery())?;
    let is_valid = address.scheme() == "http"
        && address.host_str() == Some("127.0.0.1")
        && address.port().is_some()
        && address.username().is_empty()
        && address.password().is_none()
        && address.path() == "/"
        && address.query().is_none()
        && address.fragment().is_none();
    if is_valid {
        Ok(())
    } else {
        Err(invalid_discovery())
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

fn invalid_discovery() -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::DiscoveryInvalid,
        "Runtime discovery 无效或不安全。",
    )
}

fn runtime_unavailable(_: reqwest::Error) -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::RuntimeUnavailable,
        "无法连接本地 Runtime。",
    )
}

fn runtime_stop_failed(_: reqwest::Error) -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::RuntimeStopFailed,
        "无法向 Runtime 发送受控停止请求。",
    )
}

fn bootstrap_error(
    code: RuntimeBootstrapErrorCode,
    message: impl Into<String>,
) -> RuntimeBootstrapError {
    RuntimeBootstrapError {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discovery_requires_private_regular_loopback_data() {
        let directory = tempdir().expect("tempdir");
        let runtime_home = directory.path().join("runtime-home");
        let run_directory = runtime_home.join("run");
        fs::create_dir_all(&run_directory).expect("run directory");
        #[cfg(unix)]
        fs::set_permissions(&run_directory, fs::Permissions::from_mode(0o700))
            .expect("private run directory");
        let path = runtime_home.join(DISCOVERY_RELATIVE_PATH);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .expect("discovery");
        write!(
            file,
            "{}",
            serde_json::json!({
                "address": "http://127.0.0.1:43121",
                "instance_id": "instance-1",
                "access_token": "a".repeat(43),
                "pid": std::process::id()
            })
        )
        .expect("write");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private file");

        let discovery = read_discovery(&runtime_home).expect("safe discovery");
        validate_discovery(&discovery).expect("valid discovery");

        #[cfg(unix)]
        {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("public file");
            assert!(read_discovery(&runtime_home).is_err());
        }
    }

    #[test]
    fn discovery_rejects_non_loopback_and_url_credentials() {
        let fixture = |address: &str| RuntimeDiscovery {
            address: address.to_owned(),
            instance_id: "instance-1".to_owned(),
            access_token: "a".repeat(43),
            pid: 1,
        };
        assert!(validate_discovery(&fixture("http://192.168.1.5:9000")).is_err());
        assert!(validate_discovery(&fixture("http://user@127.0.0.1:9000")).is_err());
        assert!(validate_discovery(&fixture("https://127.0.0.1:9000")).is_err());
    }

    #[test]
    fn executable_resolution_uses_an_explicit_absolute_development_override() {
        let executable = std::env::current_exe().expect("test executable");
        let coordinator = RuntimeBootstrapCoordinator::new(
            std::env::temp_dir().join("ez-assistant-test-home"),
            executable.clone(),
        )
        .expect("coordinator");
        assert_eq!(coordinator.runtime_executable, executable);
    }
}
