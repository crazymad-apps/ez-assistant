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
    RuntimeHostCapabilities, RuntimeHostFeature, SessionId, ShutdownRuntimeRequest, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri_plugin_opener::OpenerExt;
use thiserror::Error;
use url::Url;

use crate::desktop_lifecycle::{DesktopLifecycleCoordinator, NativeRuntimeState};

const DISCOVERY_RELATIVE_PATH: &str = "run/runtime.json";
const MAX_DISCOVERY_BYTES: u64 = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
// 冷启动需要完成本地服务初始化；短连接超时与整个进程的就绪等待分别限制。
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const REQUIRED_FEATURES: &[RuntimeHostFeature] = &[
    RuntimeHostFeature::StartupDiagnostics,
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

#[derive(Clone, Deserialize)]
struct RuntimeDiscovery {
    address: String,
    instance_id: String,
    access_token: String,
    pid: u32,
    #[serde(default)]
    executable_path: Option<PathBuf>,
    #[serde(default)]
    executable_sha256: Option<String>,
}

/// 只在 invoke 返回值和 RuntimeClient 私有闭包之间短暂存在的连接凭据。
#[derive(Clone, Serialize)]
pub(crate) struct RuntimeBootstrap {
    pub(crate) base_url: String,
    pub(crate) instance_id: String,
    pub(crate) access_token: String,
    pub(crate) capabilities: RuntimeHostCapabilities,
    pub(crate) started_runtime: bool,
}

impl std::fmt::Debug for RuntimeDiscovery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeDiscovery")
            .field("address", &self.address)
            .field("instance_id", &self.instance_id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RuntimeBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeBootstrap")
            .field("base_url", &self.base_url)
            .field("instance_id", &self.instance_id)
            .field("capabilities", &self.capabilities)
            .field("started_runtime", &self.started_runtime)
            .finish_non_exhaustive()
    }
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
    pub(crate) fn for_application(development: bool) -> Self {
        let runtime_home = application_runtime_home(
            std::env::var_os("EZ_ASSISTANT_RUNTIME_HOME").map(PathBuf::from),
            dirs::home_dir(),
            development || cfg!(debug_assertions),
        );
        let runtime_executable = resolve_runtime_executable().unwrap_or_default();
        Self {
            runtime_home,
            runtime_executable,
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
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
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
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
            runtime_home: crate::runtime_source::canonical_home(&runtime_home)
                .ok_or_else(invalid_discovery)?,
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
            Ok(bootstrap) => {
                if cfg!(debug_assertions) {
                    self.verify_development_origin(&bootstrap.base_url).await?;
                }
                return Ok(bootstrap);
            }
            Err(error) if matches!(error.code, RuntimeBootstrapErrorCode::ComponentMismatch) => {
                return Err(error);
            }
            Err(_) => {}
        }

        self.launch_from(&self.runtime_executable).await?;
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        let mut delay = POLL_INTERVAL;
        loop {
            match self.discover(true).await {
                Ok(bootstrap) => {
                    if cfg!(debug_assertions) {
                        self.verify_development_origin(&bootstrap.base_url).await?;
                    }
                    return Ok(bootstrap);
                }
                Err(error)
                    if matches!(error.code, RuntimeBootstrapErrorCode::ComponentMismatch) =>
                {
                    return Err(error);
                }
                Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeUnavailable,
                    "Runtime 未能在限定时间内启动，请检查配置后重试。",
                ));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), RuntimeBootstrapError> {
        let (bootstrap, original) = self.discover_target(false).await?;
        self.send_command_to(
            &bootstrap,
            "desktop-stop-runtime",
            RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default()),
        )
        .await?;
        self.wait_for_instance_to_stop(&original).await
    }

    pub(crate) async fn restart(&self) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        let (previous, original) = self.discover_target(false).await?;
        let source = original
            .executable_path
            .as_ref()
            .ok_or_else(source_unavailable)?;
        let digest = original
            .executable_sha256
            .as_ref()
            .ok_or_else(source_unavailable)?;
        let version = assistant_protocol::ClientCompatibility {
            version: previous.capabilities.runtime_version.clone(),
            min_compatible_version: previous.capabilities.min_compatible_version.clone(),
        };
        crate::runtime_source::verify(source, digest, &version)
            .await
            .map_err(|_| source_unavailable())?;
        self.send_command_to(
            &previous,
            "desktop-restart-runtime",
            RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default()),
        )
        .await?;
        self.wait_for_instance_to_stop(&original).await?;
        crate::runtime_source::verify(source, digest, &version)
            .await
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStartFailed,
                    "原 Host 已停止，但原启动来源已变化或不可用；未改用其他版本。",
                )
            })?;
        self.launch_from(source).await?;
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        let mut delay = POLL_INTERVAL;
        loop {
            match self.discover_target(true).await {
                Ok((current, discovery)) if current.instance_id != previous.instance_id => {
                    if discovery.executable_path.as_ref() != Some(source)
                        || discovery.executable_sha256.as_ref() != Some(digest)
                    {
                        return Err(instance_changed());
                    }
                    if cfg!(debug_assertions) {
                        self.verify_development_origin(&current.base_url).await?;
                    }
                    return Ok(current);
                }
                Err(error)
                    if matches!(error.code, RuntimeBootstrapErrorCode::ComponentMismatch) =>
                {
                    return Err(error);
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeUnavailable,
                    "Runtime 重启后未能在限定时间内就绪。",
                ));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
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
            .headers(crate::runtime_compatibility::headers())
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
        let bootstrap = self.discover(false).await?;
        self.send_command_to(&bootstrap, request_id, command).await
    }

    async fn send_command_to(
        &self,
        bootstrap: &RuntimeBootstrap,
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

        let response = self
            .http
            .post(format!("{}/commands", bootstrap.base_url))
            .timeout(CONTROL_COMMAND_TIMEOUT)
            .bearer_auth(&bootstrap.access_token)
            .headers(crate::runtime_compatibility::headers())
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
        original: &RuntimeDiscovery,
    ) -> Result<(), RuntimeBootstrapError> {
        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
        let mut delay = POLL_INTERVAL;
        loop {
            match read_discovery(&self.runtime_home) {
                Ok(discovery)
                    if discovery.instance_id != original.instance_id
                        || discovery.access_token != original.access_token
                        || discovery.pid != original.pid =>
                {
                    return Err(instance_changed());
                }
                Err(error)
                    if !std::fs::symlink_metadata(
                        self.runtime_home.join(DISCOVERY_RELATIVE_PATH),
                    )
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Err(error);
                }
                _ => {}
            }
            if !process_is_alive(original.pid) && instance_lock_released(&self.runtime_home)? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStopFailed,
                    "Runtime 未能在限定时间内受控停止，未强制结束或启动其他实例。",
                ));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }

    async fn discover(
        &self,
        started_runtime: bool,
    ) -> Result<RuntimeBootstrap, RuntimeBootstrapError> {
        self.discover_target(started_runtime)
            .await
            .map(|(bootstrap, _)| bootstrap)
    }

    async fn discover_target(
        &self,
        started_runtime: bool,
    ) -> Result<(RuntimeBootstrap, RuntimeDiscovery), RuntimeBootstrapError> {
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
        if missing.is_some()
            || !capabilities.sse
            || assistant_protocol::check_compatibility(
                Some(&assistant_protocol::ClientCompatibility::current()),
                &assistant_protocol::ClientCompatibility {
                    version: capabilities.runtime_version.clone(),
                    min_compatible_version: capabilities.min_compatible_version.clone(),
                },
            )
            .is_err()
        {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::ComponentMismatch,
                "Host 与 Desktop 软件版本不兼容或缺少所需能力，请更新对应应用。",
            ));
        }
        Ok((
            RuntimeBootstrap {
                base_url: discovery.address.clone(),
                instance_id: discovery.instance_id.clone(),
                access_token: discovery.access_token.clone(),
                capabilities,
                started_runtime,
            },
            discovery,
        ))
    }

    // 原生健康检查没有浏览器 Origin，不能证明 Vite 页面也能连接复用的 Host。
    // 仅检查浏览器传输是否可用；无自动凭据的请求接受通配符或精确来源。
    async fn verify_development_origin(&self, address: &str) -> Result<(), RuntimeBootstrapError> {
        const ORIGIN: &str = "http://localhost:1420";
        let response = self
            .http
            .request(reqwest::Method::OPTIONS, format!("{address}/commands"))
            .header("Origin", ORIGIN)
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "authorization,content-type,x-ez-client-version,x-ez-min-compatible-version",
            )
            .send()
            .await
            .map_err(runtime_unavailable)?;
        if !response.status().is_success()
            || !matches!(
                response
                    .headers()
                    .get("Access-Control-Allow-Origin")
                    .and_then(|value| value.to_str().ok()),
                Some(ORIGIN | "*")
            )
        {
            return Err(bootstrap_error(
                RuntimeBootstrapErrorCode::ComponentMismatch,
                "当前 Host 未允许开发页面的跨源请求，请更新 Host 后重试。",
            ));
        }
        Ok(())
    }

    async fn verify_endpoint(
        &self,
        discovery: &RuntimeDiscovery,
    ) -> Result<RuntimeHostCapabilities, RuntimeBootstrapError> {
        // 端点可达即交给客户端展示初始化状态；不能因数据库尚未 Ready 再次 launch。
        self.http
            .get(format!("{}/capabilities", discovery.address))
            .bearer_auth(&discovery.access_token)
            .headers(crate::runtime_compatibility::headers())
            .send()
            .await
            .map_err(runtime_unavailable)?
            .error_for_status()
            .map_err(runtime_unavailable)?
            .json::<RuntimeHostCapabilities>()
            .await
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::ComponentMismatch,
                    "Host 缺少有效的软件版本声明，请更新 Host。",
                )
            })
    }

    async fn launch_from(&self, executable: &Path) -> Result<(), RuntimeBootstrapError> {
        if !executable.is_file() {
            return Err(source_unavailable());
        }
        let mut child = tokio::process::Command::new(executable)
            .arg("launch")
            .arg("--runtime-home")
            .arg(&self.runtime_home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStartFailed,
                    "无法启动 Host 短启动器。",
                )
            })?;
        let status = tokio::time::timeout(CONNECT_TIMEOUT, child.wait())
            .await
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStartFailed,
                    "Host 启动器等待超时，结果待查询；没有停止已启动的 Host。",
                )
            })?
            .map_err(|_| {
                bootstrap_error(
                    RuntimeBootstrapErrorCode::RuntimeStartFailed,
                    "无法读取 Host 启动器结果。",
                )
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(bootstrap_error(
                RuntimeBootstrapErrorCode::RuntimeStartFailed,
                "Host 启动器返回失败。",
            ))
        }
    }
}

/// 开发运行默认隔离；显式覆盖无效时失败关闭，绝不回退到已安装应用的数据目录。
fn application_runtime_home(
    override_path: Option<PathBuf>,
    user_home: Option<PathBuf>,
    development: bool,
) -> PathBuf {
    if let Some(path) = override_path {
        return if path.is_absolute() {
            crate::runtime_source::canonical_home(&path).unwrap_or_default()
        } else {
            PathBuf::new()
        };
    }
    let directory = if development {
        ".ez-assistant-dev"
    } else {
        ".ez-assistant"
    };
    user_home
        .and_then(|home| crate::runtime_source::canonical_home(&home.join(directory)))
        .unwrap_or_default()
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
    if !runtime_home.is_absolute() {
        return Err(bootstrap_error(
            RuntimeBootstrapErrorCode::RuntimeHomeUnavailable,
            "Runtime Home 必须是绝对路径。",
        ));
    }
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
        let parent_metadata = fs::symlink_metadata(parent).map_err(|_| invalid_discovery())?;
        if !parent_metadata.is_dir()
            || parent_metadata.mode() & 0o077 != 0
            || metadata.uid() != parent_metadata.uid()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
        {
            return Err(invalid_discovery());
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&path).map_err(|_| invalid_discovery())?;
    let opened = file.metadata().map_err(|_| invalid_discovery())?;
    #[cfg(unix)]
    if metadata.dev() != opened.dev()
        || metadata.ino() != opened.ino()
        || opened.mode() & 0o077 != 0
        || metadata.uid() != opened.uid()
    {
        return Err(invalid_discovery());
    }
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
    let is_valid = matches!(address.scheme(), "http" | "https")
        && address.host_str() == Some("127.0.0.1")
        && address.port_or_known_default().is_some_and(|port| port > 0)
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

fn source_unavailable() -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable,
        "原 Host 启动来源缺失、已改变或版本不符；未停止当前 Host。",
    )
}

fn instance_changed() -> RuntimeBootstrapError {
    bootstrap_error(
        RuntimeBootstrapErrorCode::RuntimeStopFailed,
        "操作期间 Host 实例已变化；保留当前实例，请重新查询。",
    )
}

fn instance_lock_released(home: &Path) -> Result<bool, RuntimeBootstrapError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(home.join("run/runtime.lock"))
        .map_err(|_| invalid_discovery())?;
    let metadata = file.metadata().map_err(|_| invalid_discovery())?;
    if !metadata.is_file() {
        return Err(invalid_discovery());
    }
    #[cfg(unix)]
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(invalid_discovery());
    }
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(_) => Err(invalid_discovery()),
    }
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
    fn application_home_isolated_in_development_and_invalid_override_never_falls_back() {
        let home = Some(PathBuf::from("/test-user"));
        assert_eq!(
            application_runtime_home(None, home.clone(), true),
            PathBuf::from("/test-user/.ez-assistant-dev")
        );
        assert_eq!(
            application_runtime_home(None, home.clone(), false),
            PathBuf::from("/test-user/.ez-assistant")
        );
        assert_eq!(
            application_runtime_home(
                Some(PathBuf::from("/tmp/isolated-host")),
                home.clone(),
                false
            ),
            crate::runtime_source::canonical_home(Path::new("/tmp/isolated-host")).unwrap()
        );
        assert!(
            application_runtime_home(Some(PathBuf::from("relative")), home, false)
                .as_os_str()
                .is_empty()
        );
    }

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
            executable_path: None,
            executable_sha256: None,
        };
        assert!(validate_discovery(&fixture("http://192.168.1.5:9000")).is_err());
        assert!(validate_discovery(&fixture("http://user@127.0.0.1:9000")).is_err());
        assert!(validate_discovery(&fixture("https://127.0.0.1:9000")).is_ok());
        assert!(validate_discovery(&fixture("https://192.168.1.5:9000")).is_err());
    }

    #[tokio::test]
    async fn development_preflight_accepts_explicit_or_wildcard_origin_without_credentials() {
        use std::io::{Read as _, Write as _};
        for (status, allow_origin, expected) in [
            ("403 Forbidden", "", false),
            ("204 No Content", "", false),
            ("204 No Content", "Access-Control-Allow-Origin: *\r\n", true),
            (
                "204 No Content",
                "Access-Control-Allow-Origin: https://other.test\r\n",
                false,
            ),
            (
                "204 No Content",
                "Access-Control-Allow-Origin: http://localhost:1420\r\n",
                true,
            ),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let length = stream.read(&mut chunk).unwrap();
                    assert!(length > 0);
                    request.extend_from_slice(&chunk[..length]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.starts_with("options /commands"));
                assert!(request.contains("origin: http://localhost:1420"));
                stream.write_all(format!("HTTP/1.1 {status}\r\n{allow_origin}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
            });
            let home = tempfile::tempdir().unwrap();
            let coordinator = RuntimeBootstrapCoordinator::new(
                home.path().to_owned(),
                std::env::current_exe().unwrap(),
            )
            .unwrap();
            let result = coordinator.verify_development_origin(&address).await;
            if expected {
                assert!(result.is_ok());
            } else {
                assert!(matches!(
                    result.unwrap_err().code,
                    RuntimeBootstrapErrorCode::ComponentMismatch
                ));
            }
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn reachable_diagnostic_host_is_reused_without_launch_or_health_readiness_timeout() {
        use std::io::Read as _;
        for (version, minimum, accepted) in [
            ("0.25.2", Some("0.25.2"), true),
            ("0.25.3", Some("0.25.2"), true),
            ("0.25.1", Some("0.25.1"), false),
            ("0.25.3", Some("0.25.3"), false),
            ("0.25.1", None, false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                for _ in 0..if accepted { 2 } else { 1 } {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        let mut chunk = [0; 2048];
                        let length = stream.read(&mut chunk).unwrap();
                        assert!(length > 0);
                        bytes.extend_from_slice(&chunk[..length]);
                    }
                    let request = String::from_utf8(bytes).unwrap();
                    let (status, headers, body) = if request.starts_with("GET /capabilities ") {
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("authorization: bearer")
                        );
                        ("200 OK", "Content-Type: application/json\r\n", {
                            let mut capabilities = serde_json::json!({
                                "runtime_version": version, "max_command_bytes": 1024,
                                "max_attachment_bytes": null, "sse": true, "streaming_upload": true,
                                "features": REQUIRED_FEATURES,
                            });
                            if let Some(minimum) = minimum {
                                capabilities["min_compatible_version"] = minimum.into();
                            }
                            capabilities.to_string()
                        })
                    } else {
                        assert!(
                            request.starts_with("OPTIONS /commands "),
                            "must not wait for health before returning discovery"
                        );
                        (
                            "204 No Content",
                            "Access-Control-Allow-Origin: http://localhost:1420\r\n",
                            String::new(),
                        )
                    };
                    write!(stream, "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            let home = tempdir().unwrap();
            let directory = home.path().join("run");
            fs::create_dir(&directory).unwrap();
            let path = directory.join("runtime.json");
            fs::write(&path, serde_json::to_vec(&serde_json::json!({
            "address":address, "instance_id":"initializing-instance", "access_token":"a".repeat(43), "pid":std::process::id(),
        })).unwrap()).unwrap();
            #[cfg(unix)]
            {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            let coordinator = RuntimeBootstrapCoordinator::new(
                home.path().to_owned(),
                home.path().join("no-executable"),
            )
            .unwrap();
            let connected = coordinator.bootstrap().await;
            if accepted {
                let connected = connected.unwrap();
                assert_eq!(connected.instance_id, "initializing-instance");
                assert!(!connected.started_runtime);
            } else {
                assert!(matches!(
                    connected.unwrap_err().code,
                    RuntimeBootstrapErrorCode::ComponentMismatch
                ));
            }
            server.join().unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "显式连接已有隔离 Host，不启动应用或访问生产 Home"]
    async fn isolated_host_uses_native_software_version_admission_without_launching() {
        let home = PathBuf::from(
            std::env::var_os("EZ_ASSISTANT_TEST_RUNTIME_HOME").expect("isolated Host home"),
        )
        .canonicalize()
        .unwrap();
        assert!(home.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert_eq!(
            fs::read_to_string(home.join("m2-isolated-validation")).unwrap(),
            "v0.25.2"
        );
        let before = fs::read(home.join("run/runtime.json")).unwrap();
        let coordinator =
            RuntimeBootstrapCoordinator::new(home.clone(), home.join("no-executable")).unwrap();
        let connected = coordinator.bootstrap().await;
        if std::env::var_os("EZ_ASSISTANT_TEST_LEGACY_HOST").is_some() {
            assert!(matches!(
                connected.unwrap_err().code,
                RuntimeBootstrapErrorCode::ComponentMismatch
            ));
        } else {
            let connected = connected.unwrap();
            assert!(!connected.started_runtime);
            assert_eq!(
                connected.capabilities.runtime_version,
                assistant_protocol::SOFTWARE_VERSION
            );
        }
        assert_eq!(fs::read(home.join("run/runtime.json")).unwrap(), before);
    }

    #[tokio::test]
    async fn shutdown_uses_captured_credentials_even_when_discovery_changes() {
        use std::io::Read as _;
        let home = tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut chunk = [0; 2048];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let request = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
            assert!(request.starts_with("post /commands "));
            assert!(request.contains("authorization: bearer captured-secret"));
            let body = r#"{"result":{"scope":"runtime","payload":{"type":"shutdown_runtime","payload":{"lifecycle":"shutting_down"}}}}"#;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let coordinator =
            RuntimeBootstrapCoordinator::new(home.path().to_owned(), home.path().join("absent"))
                .unwrap();
        let captured = RuntimeBootstrap {
            base_url: address,
            instance_id: "captured".into(),
            access_token: "captured-secret".into(),
            started_runtime: false,
            capabilities: serde_json::from_value(serde_json::json!({
                "runtime_version":"0.25.2","min_compatible_version":"0.25.2",
                "max_command_bytes":1024,"max_attachment_bytes":null,"sse":true,
                "streaming_upload":true,"features":REQUIRED_FEATURES,
            }))
            .unwrap(),
        };
        fs::create_dir(home.path().join("run")).unwrap();
        fs::write(
            home.path().join("run/runtime.json"),
            "a concurrent replacement is not this operation's target",
        )
        .unwrap();
        assert!(matches!(
            coordinator
                .send_command_to(
                    &captured,
                    "test-stop",
                    RuntimeCommand::ShutdownRuntime(ShutdownRuntimeRequest::default())
                )
                .await
                .unwrap(),
            RuntimeCommandResult::ShutdownRuntime(_)
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn missing_discovery_is_not_proof_of_exit_and_successor_is_preserved() {
        let home = tempdir().unwrap();
        let run = home.path().join("run");
        fs::create_dir(&run).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator =
            RuntimeBootstrapCoordinator::new(home.path().to_owned(), home.path().join("absent"))
                .unwrap();
        let original = RuntimeDiscovery {
            address: "http://127.0.0.1:7240".into(),
            instance_id: "original".into(),
            access_token: "a".repeat(43),
            pid: std::process::id(),
            executable_path: None,
            executable_sha256: None,
        };
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                coordinator.wait_for_instance_to_stop(&original)
            )
            .await
            .is_err()
        );
        let mut successor = original.clone();
        successor.instance_id = "successor".into();
        let path = run.join("runtime.json");
        let content = serde_json::to_vec(&serde_json::json!({
            "address": successor.address, "instance_id": successor.instance_id,
            "access_token": successor.access_token, "pid": successor.pid,
        }))
        .unwrap();
        fs::write(&path, &content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            coordinator
                .wait_for_instance_to_stop(&original)
                .await
                .unwrap_err()
                .code,
            RuntimeBootstrapErrorCode::RuntimeStopFailed
        ));
        assert_eq!(fs::read(path).unwrap(), content);
    }

    /// Explicit entry for an already prepared, marked non-production M3 fixture.
    #[tokio::test]
    #[ignore = "显式连接带 M3 标记的临时 Host；不启动 Desktop GUI"]
    async fn isolated_native_restart_preserves_the_running_host_source() {
        let home =
            PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_RUNTIME_HOME").expect("M3 fixture"))
                .canonicalize()
                .unwrap();
        assert!(home.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert_eq!(
            fs::read_to_string(home.join("m3-isolated-validation")).unwrap(),
            "v0.25.2"
        );
        let before = read_discovery(&home).unwrap();
        let source = before
            .executable_path
            .as_ref()
            .unwrap()
            .canonicalize()
            .unwrap();
        assert!(
            source.starts_with(home.parent().unwrap()),
            "source must be this fixture's private copy"
        );
        let coordinator =
            RuntimeBootstrapCoordinator::new(home.clone(), home.join("not-the-original-host"))
                .unwrap();
        let restarted = coordinator.restart().await.unwrap();
        let after = read_discovery(&home).unwrap();
        assert_ne!(after.instance_id, before.instance_id);
        assert_eq!(after.executable_path, before.executable_path);
        assert_eq!(after.executable_sha256, before.executable_sha256);
        assert_eq!(restarted.instance_id, after.instance_id);
        // The native bootstrap hands Starting/Unavailable to the existing UI lifecycle; do not
        // infer business readiness from discovery. Require actual health for this acceptance test.
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        loop {
            let response = coordinator
                .http
                .get(format!("{}/health", restarted.base_url))
                .bearer_auth(&restarted.access_token)
                .headers(crate::runtime_compatibility::headers())
                .send()
                .await
                .unwrap();
            let health = response
                .error_for_status()
                .unwrap()
                .json::<assistant_protocol::RuntimeHostHealth>()
                .await
                .unwrap();
            assert_ne!(
                health.status,
                assistant_protocol::RuntimeHostHealthStatus::Unavailable,
                "restarted Host reported an initialization failure"
            );
            if health.status == assistant_protocol::RuntimeHostHealthStatus::Ready {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "restarted Host did not become Ready"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        coordinator.shutdown().await.unwrap();
        assert!(!process_is_alive(after.pid));
        assert!(instance_lock_released(&home).unwrap());
    }

    #[tokio::test]
    #[ignore = "显式连接原来源已移走或替换的 M3 临时 Host"]
    async fn isolated_native_restart_rejects_unavailable_source_without_stopping() {
        let home =
            PathBuf::from(std::env::var_os("EZ_ASSISTANT_TEST_RUNTIME_HOME").expect("M3 fixture"))
                .canonicalize()
                .unwrap();
        assert!(home.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert_eq!(
            fs::read_to_string(home.join("m3-isolated-validation")).unwrap(),
            "v0.25.2"
        );
        let before_bytes = fs::read(home.join(DISCOVERY_RELATIVE_PATH)).unwrap();
        let before = read_discovery(&home).unwrap();
        assert!(
            before
                .executable_path
                .as_ref()
                .unwrap()
                .starts_with(home.parent().unwrap())
        );
        let coordinator =
            RuntimeBootstrapCoordinator::new(home.clone(), home.join("not-a-fallback")).unwrap();
        assert!(matches!(
            coordinator.restart().await.unwrap_err().code,
            RuntimeBootstrapErrorCode::RuntimeExecutableUnavailable
        ));
        assert!(process_is_alive(before.pid));
        assert_eq!(
            fs::read(home.join(DISCOVERY_RELATIVE_PATH)).unwrap(),
            before_bytes
        );
        assert_eq!(
            coordinator.discover(false).await.unwrap().instance_id,
            before.instance_id
        );
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
