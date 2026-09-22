//! Host 私有访问服务：串行配置、共享登录表与同一监听上的远程访问开关。

mod config;
mod credentials;
pub(crate) mod enterprise;
pub(crate) mod offline;
#[cfg(test)]
mod tests;

use std::sync::{Arc, RwLock};

use assistant_protocol::{
    HostAccessCommand, HostAccessConfiguration, HostAccessStatus, HostListenerState,
    RuntimeErrorCode, RuntimeErrorInfo, SecretValue,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{config_source::LocalConfigSource, http::HttpState};
use config::{AccessDocument, load, same_endpoint, save, validate};
pub(crate) use credentials::{AccessPermit, Credentials};

pub(crate) fn validate_layout_configuration(contents: &str) -> Result<(), AccessError> {
    config::validate_layout_configuration(contents)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AccessError {
    #[error(transparent)]
    Center(#[from] enterprise::CenterError),
    #[error("登录已失效，请重新登录。")]
    Unauthorized,
    #[error("操作过于频繁，请稍后重试。")]
    Busy,
    #[error("配置已被修改，请重新载入后保存。")]
    Conflict,
    #[error("Host 访问服务暂不可用。")]
    Unavailable,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Host 端口 {0} 已被占用，请在 host.toml 的 host_access.port 中设置其他端口后重试。")]
    PortInUse(u16),
}

impl AccessError {
    pub(crate) fn protocol_info(&self) -> RuntimeErrorInfo {
        let code = match self {
            Self::Center(_) => RuntimeErrorCode::ConfigurationUnavailable,
            Self::Conflict => RuntimeErrorCode::ConfigurationConflict,
            Self::Unavailable | Self::Busy => RuntimeErrorCode::ConfigurationUnavailable,
            Self::Unauthorized => RuntimeErrorCode::OperationNotAllowed,
            Self::Invalid(_) | Self::PortInUse(_) => RuntimeErrorCode::InvalidRequest,
        };
        RuntimeErrorInfo::new(code, self.to_string())
    }
}

struct AccessRequest {
    command: Option<HostAccessCommand>,
    permit: AccessPermit,
    reply: oneshot::Sender<Result<HostAccessStatus, AccessError>>,
}

struct CenterBindingRequest {
    url: String,
    id: String,
    reply: oneshot::Sender<Result<(), AccessError>>,
}

#[derive(Clone)]
pub(crate) struct HostAccessHandle {
    pub(crate) protected_files: Arc<RwLock<Vec<std::path::PathBuf>>>,
    pub(crate) credentials: Arc<Credentials>,
    commands: mpsc::Sender<AccessRequest>,
    remote: Arc<RwLock<RemoteAccess>>,
    center: Arc<RwLock<Option<enterprise::CenterClient>>>,
    binding: mpsc::Sender<CenterBindingRequest>,
}

impl HostAccessHandle {
    pub(crate) fn center(&self) -> Option<enterprise::CenterClient> {
        self.center
            .read()
            .expect("center configuration poisoned")
            .clone()
    }
    pub(crate) fn mode(&self) -> assistant_protocol::HostMode {
        if self.center().is_some() {
            assistant_protocol::HostMode::Enterprise
        } else {
            assistant_protocol::HostMode::Personal
        }
    }
    async fn bind_center(&self, url: String, id: String) -> Result<(), AccessError> {
        let (reply, receive) = oneshot::channel();
        self.binding
            .try_send(CenterBindingRequest { url, id, reply })
            .map_err(|_| AccessError::Busy)?;
        receive.await.map_err(|_| AccessError::Unavailable)?
    }
    pub(crate) async fn command(
        &self,
        command: Option<HostAccessCommand>,
        permit: AccessPermit,
    ) -> Result<HostAccessStatus, AccessError> {
        permit.check()?;
        let (reply, receive) = oneshot::channel();
        self.commands
            .try_send(AccessRequest {
                command,
                permit,
                reply,
            })
            .map_err(|_| AccessError::Busy)?;
        receive.await.map_err(|_| AccessError::Unavailable)?
    }
}

pub(crate) struct HostAccessService {
    protected_files: Arc<RwLock<Vec<std::path::PathBuf>>>,
    source: Arc<LocalConfigSource>,
    credentials: Arc<Credentials>,
    commands: mpsc::Receiver<AccessRequest>,
    remote: Arc<RwLock<RemoteAccess>>,
    configuration: HostAccessConfiguration,
    center: Arc<RwLock<Option<enterprise::CenterClient>>>,
    binding: mpsc::Receiver<CenterBindingRequest>,
    identity_restart_required: bool,
}

impl HostAccessService {
    pub(crate) fn new(source: Arc<LocalConfigSource>) -> (Self, HostAccessHandle) {
        let (commands, receive) = mpsc::channel(16);
        let credentials = Arc::new(Credentials::new());
        let remote = Arc::new(RwLock::new(RemoteAccess::default()));
        let center = Arc::new(RwLock::new(None));
        let (binding, bindings) = mpsc::channel(16);
        let protected_files = Arc::new(RwLock::new(Vec::new()));
        let handle = HostAccessHandle {
            protected_files: protected_files.clone(),
            credentials: credentials.clone(),
            commands,
            remote: remote.clone(),
            center: center.clone(),
            binding,
        };
        (
            Self {
                protected_files,
                source,
                credentials,
                commands: receive,
                remote,
                configuration: HostAccessConfiguration::default(),
                center,
                binding: bindings,
                identity_restart_required: false,
            },
            handle,
        )
    }

    /// 本地启动初始化在实例锁后调用；输入和文件 CAS 与设置入口共用实现。
    pub(crate) async fn initialize_password(
        &self,
        password: SecretValue,
    ) -> Result<(), AccessError> {
        let mut document = load(self.source.as_ref()).await?;
        document.access.password_hash = Some(self.credentials.hash_password(password).await?);
        save(self.source.as_ref(), document).await?;
        Ok(())
    }

    /// 在绑定唯一端口前冻结监听配置，凭据与访问策略仍由本服务维护。
    pub(crate) async fn prepare(&mut self) -> Result<HostAccessConfiguration, AccessError> {
        let document = load(self.source.as_ref()).await?;
        if document.document.get("version").is_some() {
            let host = crate::host_configuration::parse(&document.document.to_string())
                .map_err(AccessError::Invalid)?;
            if host.mode == assistant_protocol::HostMode::Enterprise {
                let config = host.enterprise.ok_or(AccessError::Unavailable)?;
                *self.center.write().expect("center configuration poisoned") =
                    Some(enterprise::CenterClient::new(config.center_url)?);
            }
        }
        validate(&document.access.public)?;
        self.configuration = document.access.public.clone();
        self.apply_policy(&document)?;
        Ok(self.configuration.clone())
    }

    pub(crate) async fn run_until(
        mut self,
        state: HttpState,
        shutdown: CancellationToken,
    ) -> Result<(), AccessError> {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                request = self.binding.recv() => {
                    let Some(request) = request else { break; };
                    let result = self.bind_center(&request.url, &request.id).await;
                    let _ = request.reply.send(result);
                },
                request = self.commands.recv() => {
                    let Some(request) = request else { break; };
                    let result = self.execute(request.command, &request.permit, &state).await;
                    let _ = request.reply.send(result);
                },
            }
        }
        Ok(())
    }

    /// 与 Host 配置操作共用 actor；文件 CAS 检查外部改动，不覆盖另一个中心的绑定。
    async fn bind_center(&self, url: &str, id: &str) -> Result<(), AccessError> {
        let mut document = load(self.source.as_ref()).await?;
        let host = crate::host_configuration::parse(&document.document.to_string())
            .map_err(AccessError::Invalid)?;
        let center = host.enterprise.ok_or(AccessError::Unavailable)?;
        if host.mode != assistant_protocol::HostMode::Enterprise
            || center.center_url != url
            || center.center_id.as_deref().is_some_and(|bound| bound != id)
        {
            return Err(enterprise::CenterError::IdentityMismatch.into());
        }
        if center.center_id.is_none() {
            document.document["enterprise"]["center_id"] = toml_edit::value(id);
            save(self.source.as_ref(), document).await?;
        }
        Ok(())
    }

    async fn execute(
        &mut self,
        command: Option<HostAccessCommand>,
        permit: &AccessPermit,
        _state: &HttpState,
    ) -> Result<HostAccessStatus, AccessError> {
        permit.check()?;
        let Some(command) = command else {
            return self.reload().await;
        };
        let mut document = load(self.source.as_ref()).await?;
        match command {
            HostAccessCommand::GetStatus => {}
            HostAccessCommand::SetPassword {
                expected_revision,
                password,
            } => {
                check_revision(&document, &expected_revision)?;
                document.access.password_hash =
                    Some(self.credentials.hash_password(password).await?);
                permit.check()?;
                document = save(self.source.as_ref(), document).await?;
                self.credentials
                    .replace_password(document.access.password_hash.clone());
            }
            HostAccessCommand::Configure {
                expected_revision,
                configuration,
                mode,
                center_url,
                clear_center_binding,
            } => {
                check_revision(&document, &expected_revision)?;
                validate(&configuration)?;
                // 个人 Web 的旧访问设置权限不扩展为切换整个 Host 的身份模式。
                if (mode.is_some() || center_url.is_some() || clear_center_binding.is_some())
                    && !permit.native
                {
                    return Err(AccessError::Unauthorized);
                }
                let previous_identity = config::identity_fields(&document.document);
                config::configure_identity(
                    &mut document.document,
                    mode,
                    center_url.as_deref(),
                    clear_center_binding.unwrap_or(false),
                )?;
                let identity_changed =
                    previous_identity != config::identity_fields(&document.document);
                if self.remote_enabled() && !same_endpoint(&configuration, &self.configuration) {
                    return Err(AccessError::Invalid("请先关闭非本地访问，再修改监听配置。"));
                }
                if identity_changed && configuration.remote_enabled {
                    return Err(AccessError::Invalid(
                        "请先关闭非本地访问再保存模式或中心变更，重启后可重新开启。",
                    ));
                }
                if configuration.remote_enabled
                    && document.access.password_hash.is_none()
                    && self
                        .center
                        .read()
                        .expect("center configuration poisoned")
                        .is_none()
                {
                    return Err(AccessError::Invalid("请先设置密码，再开启非本地访问。"));
                }
                if configuration.remote_enabled
                    && !same_endpoint(&configuration, &self.configuration)
                {
                    return Err(AccessError::Invalid(
                        "请先保存端口、协议或证书并重启 Host，再开启非本地访问。",
                    ));
                }
                if !same_endpoint(&configuration, &self.configuration) {
                    crate::server::tls_configuration(&configuration).await?;
                    if configuration.port != self.configuration.port {
                        // 提前报告明显占用；这里只验证，正式监听在下次启动时建立。
                        crate::server::bind(configuration.port)?;
                    }
                }
                permit.check()?;
                document.access.public = configuration;
                document = save(self.source.as_ref(), document).await?;
                self.identity_restart_required |= identity_changed;
                self.apply_policy(&document)?;
            }
        }
        Ok(self.status(&document))
    }

    async fn reload(&mut self) -> Result<HostAccessStatus, AccessError> {
        let document = match load(self.source.as_ref()).await {
            Ok(document) => document,
            Err(error) => {
                self.credentials.replace_password(None);
                if let Ok(mut remote) = self.remote.write() {
                    remote.disable();
                }
                return Err(error);
            }
        };
        self.apply_policy(&document)?;
        Ok(self.status(&document))
    }

    /// CAS 已成功（或启动已读取配置）后发布访问策略；不重绑端口、不取消 Runtime Run。
    fn apply_policy(&mut self, document: &AccessDocument) -> Result<(), AccessError> {
        self.credentials
            .replace_password(document.access.password_hash.clone());
        let mut remote = self.remote.write().map_err(|_| AccessError::Unavailable)?;
        let configuration = &document.access.public;
        if let Err(error) = validate(configuration) {
            remote.disable();
            return Err(error);
        }
        // 生效私钥与待重启私钥同时受保护，已有用户域立即看到本机管理配置的变更。
        *self
            .protected_files
            .write()
            .map_err(|_| AccessError::Unavailable)? = self
            .configuration
            .tls_private_key
            .iter()
            .chain(configuration.tls_private_key.iter())
            .map(std::path::PathBuf::from)
            .collect();
        let enabled = configuration.remote_enabled
            && (document.access.password_hash.is_some()
                || self
                    .center
                    .read()
                    .expect("center configuration poisoned")
                    .is_some())
            && same_endpoint(configuration, &self.configuration);
        if !enabled || remote.server_names != configuration.server_names {
            remote.disable();
        }
        if enabled && !remote.enabled {
            remote.connections = CancellationToken::new();
        }
        remote.enabled = enabled;
        remote.server_names = configuration.server_names.clone();
        Ok(())
    }

    fn remote_enabled(&self) -> bool {
        self.remote.read().is_ok_and(|remote| remote.enabled)
    }

    fn status(&self, document: &AccessDocument) -> HostAccessStatus {
        let error = (document.access.public.remote_enabled
            && document.access.password_hash.is_none()
            && self
                .center
                .read()
                .expect("center configuration poisoned")
                .is_none())
        .then(|| "未设置密码，非本地访问保持关闭。".to_owned());
        let (mode, center_url, center_id) = config::identity_fields(&document.document);
        HostAccessStatus {
            mode,
            center_url,
            center_id,
            revision: document.revision.clone(),
            password_configured: document.access.password_hash.is_some(),
            configuration: document.access.public.clone(),
            listener_state: if self.remote_enabled() {
                HostListenerState::Listening
            } else if error.is_some() {
                HostListenerState::Failed
            } else {
                HostListenerState::Closed
            },
            restart_required: self.identity_restart_required
                || !same_endpoint(&document.access.public, &self.configuration),
            error,
        }
    }
}

fn check_revision(document: &AccessDocument, expected: &Option<String>) -> Result<(), AccessError> {
    if &document.revision != expected {
        return Err(AccessError::Conflict);
    }
    Ok(())
}

// 这份易失策略只描述已生效的远程接入；监听配置在启动时冻结，没有第二份业务状态。
#[derive(Default)]
struct RemoteAccess {
    enabled: bool,
    server_names: Vec<String>,
    connections: CancellationToken,
}
impl RemoteAccess {
    fn disable(&mut self) {
        self.enabled = false;
        self.connections.cancel();
    }
}
impl Drop for HostAccessService {
    fn drop(&mut self) {
        // actor 异常退出同样拒绝远程访问并结束已有流；本机入口仍可报告服务不可用。
        if let Ok(mut remote) = self.remote.write() {
            remote.disable();
        }
    }
}
impl HostAccessHandle {
    pub(crate) fn remote_connection(
        &self,
        server_name: &str,
    ) -> Result<CancellationToken, AccessError> {
        let remote = self.remote.read().map_err(|_| AccessError::Unavailable)?;
        let requested = config::normalized_server_name(server_name)?;
        // 回环地址也可能经 Docker／隧道转发到达：允许普通远程访问，不据此授予本机权限。
        // 原生凭据仍由 HTTP 层结合真实 TCP 来源校验，不按 IP 地址类别限制普通访问。
        let allowed = requested.parse::<std::net::IpAddr>().is_ok()
            || remote.server_names.iter().any(|name| {
                config::normalized_server_name(name).is_ok_and(|name| name == requested)
            });
        if !remote.enabled || !allowed {
            return Err(AccessError::Unauthorized);
        }
        Ok(remote.connections.clone())
    }
}
