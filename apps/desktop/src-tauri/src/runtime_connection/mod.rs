//! Desktop 当前连接的原生凭据与不可变请求绑定；不持有 Runtime 业务状态。
mod passwords;

use crate::runtime_bootstrap::{RuntimeBootstrap, RuntimeBootstrapCoordinator};
use assistant_protocol::{
    HostLoginRequest, HostLoginResult, HostMode, RuntimeHostCapabilities, RuntimeHostFeature,
    SecretValue,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, State,
    ipc::{CommandArg, CommandItem, InvokeError},
};
use tauri_plugin_opener::OpenerExt as _;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Serialize)]
pub(crate) struct ConnectionError {
    code: &'static str,
    message: String,
}
fn failure(code: &'static str, message: &'static str) -> ConnectionError {
    ConnectionError {
        code,
        message: message.to_owned(),
    }
}

impl ConnectionError {
    // 只附加固定阶段，不回显响应正文、密码、Token 或请求 URL。
    fn at_step(mut self, step: &'static str) -> Self {
        self.message = format!("{step}：{}", self.message);
        self
    }
}
fn stale() -> ConnectionError {
    failure("connection_changed", "连接目标已切换，请重新操作。")
}
fn unavailable() -> ConnectionError {
    failure(
        "runtime_unavailable",
        "无法连接 Runtime，请检查地址、网络及证书信任。",
    )
}

fn transport_error(error: &reqwest::Error) -> ConnectionError {
    let mut failure = if error.is_timeout() {
        failure("runtime_unavailable", "连接 Runtime 超时。")
    } else if error.is_connect() {
        failure(
            "runtime_unavailable",
            "无法建立到 Runtime 的连接，请检查网络及证书信任。",
        )
    } else {
        unavailable()
    };
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        if let Some(code) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
        {
            failure.message.push_str(&format!("（系统错误码 {code}）"));
            break;
        }
        source = cause.source();
    }
    failure
}

#[derive(Default)]
pub(crate) struct RuntimeConnection {
    selected: Mutex<Option<Selection>>,
}
struct Selection {
    id: String,
    cancellation: CancellationToken,
    context: Option<Arc<TargetContext>>,
}
struct TargetContext {
    cancellation: CancellationToken,
    bootstrap: RuntimeBootstrap,
    local: bool,
    local_proof: Option<String>,
}

/// 在 invoke 参数解析时冻结目标；后续 await 不能重新读取全局目标。
#[derive(Clone)]
pub(crate) struct RuntimeTarget {
    context: Arc<TargetContext>,
    pub(crate) cancellation: CancellationToken,
}
impl RuntimeTarget {
    pub(crate) fn same_connection(&self, other: &Self) -> bool {
        !self.cancellation.is_cancelled()
            && !other.cancellation.is_cancelled()
            && Arc::ptr_eq(&self.context, &other.context)
    }
    /// 原生浏览器创建前向已绑定 Host 核验本人；不接受 WebView 自报的用户或 Profile。
    pub(crate) async fn browser_identity(
        &self,
    ) -> Result<(HostLoginResult, bool, String), ConnectionError> {
        let bootstrap = self.bootstrap().await?;
        let response = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(stale()),
            result = http()?.get(format!("{}/auth/session", bootstrap.base_url))
                .bearer_auth(&bootstrap.access_token).headers(crate::runtime_compatibility::headers()).send() => result.map_err(|_| unavailable())?,
        };
        let identity = decode_login(response).await?;
        self.ensure_active()?;
        if identity.instance_id != bootstrap.instance_id {
            return Err(stale());
        }
        Ok((identity, self.context.local, bootstrap.base_url))
    }
    pub(crate) fn ensure_active(&self) -> Result<(), ConnectionError> {
        if self.cancellation.is_cancelled() {
            Err(stale())
        } else {
            Ok(())
        }
    }
    /// 企业普通 Token 表明用户；这份仅原生持有的独立证明表明请求确由本机管理客户端发出。
    pub(crate) fn native_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = crate::runtime_compatibility::headers();
        if let Some(proof) = &self.context.local_proof
            && let Ok(value) = proof.parse()
        {
            headers.insert("x-ez-host-bootstrap", value);
        }
        headers
    }
    pub(crate) fn ensure_local(&self) -> Result<(), ConnectionError> {
        self.ensure_active()?;
        if self.context.local {
            Ok(())
        } else {
            Err(failure(
                "local_operation_unavailable",
                "此操作仅支持本机 Runtime。",
            ))
        }
    }
    pub(crate) async fn bootstrap(&self) -> Result<RuntimeBootstrap, ConnectionError> {
        self.ensure_active()?;
        Ok(self.context.bootstrap.clone())
    }
}
impl<'a, R: tauri::Runtime> CommandArg<'a, R> for RuntimeTarget {
    fn from_command(command: CommandItem<'a, R>) -> Result<Self, InvokeError> {
        let binding = command
            .message
            .headers()
            .get("x-ez-runtime-binding")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| InvokeError::from(stale()))?;
        command
            .message
            .state_ref()
            .get::<RuntimeConnection>()
            .target(binding)
            .map_err(InvokeError::from)
    }
}
impl RuntimeConnection {
    fn begin(&self) -> Result<String, ConnectionError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| unavailable())?;
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut selected = self.selected.lock().map_err(|_| unavailable())?;
        if let Some(previous) = selected.take() {
            previous.cancellation.cancel();
        }
        *selected = Some(Selection {
            id: id.clone(),
            cancellation: CancellationToken::new(),
            context: None,
        });
        Ok(id)
    }
    fn cancellation(&self, id: &str) -> Result<CancellationToken, ConnectionError> {
        let selected = self.selected.lock().map_err(|_| unavailable())?;
        selected
            .as_ref()
            .filter(|value| value.id == id)
            .map(|value| value.cancellation.clone())
            .ok_or_else(stale)
    }
    #[cfg(test)]
    fn bind(
        &self,
        id: &str,
        bootstrap: RuntimeBootstrap,
        local: bool,
    ) -> Result<(), ConnectionError> {
        self.bind_with_proof(id, bootstrap, local, None)
    }
    fn bind_with_proof(
        &self,
        id: &str,
        bootstrap: RuntimeBootstrap,
        local: bool,
        local_proof: Option<String>,
    ) -> Result<(), ConnectionError> {
        let mut selected = self.selected.lock().map_err(|_| unavailable())?;
        let selection = selected
            .as_mut()
            .filter(|value| value.id == id)
            .ok_or_else(stale)?;
        if let Some(previous) = selection.context.take() {
            previous.cancellation.cancel();
        }
        selection.context = Some(Arc::new(TargetContext {
            cancellation: selection.cancellation.child_token(),
            bootstrap,
            local,
            local_proof,
        }));
        Ok(())
    }
    fn target(&self, id: &str) -> Result<RuntimeTarget, ConnectionError> {
        let selected = self.selected.lock().map_err(|_| unavailable())?;
        let selected = selected
            .as_ref()
            .filter(|value| value.id == id)
            .ok_or_else(stale)?;
        Ok(RuntimeTarget {
            context: selected.context.clone().ok_or_else(stale)?,
            cancellation: selected
                .context
                .as_ref()
                .ok_or_else(stale)?
                .cancellation
                .clone(),
        })
    }
}

#[derive(Serialize)]
pub(crate) struct PreparedConnection {
    bootstrap: RuntimeBootstrap,
    warning: Option<&'static str>,
}

#[tauri::command]
pub(crate) async fn begin_runtime_connection(
    connection: State<'_, RuntimeConnection>,
    resources: State<'_, crate::native_resource::NativeResourceBridge>,
    browsers: State<'_, crate::browser_resource::BrowserResourceManager>,
) -> Result<String, ConnectionError> {
    let id = connection.begin()?;
    resources.clear_target_resources();
    browsers.close_all();
    connection.cancellation(&id)?;
    Ok(id)
}

// Tauri 注入三个状态参数，其余为登录表单；不引入额外持久化配置。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn connect_runtime_target(
    app: AppHandle,
    connection: State<'_, RuntimeConnection>,
    local: State<'_, RuntimeBootstrapCoordinator>,
    binding_id: String,
    origin: Option<String>,
    password: String,
    remember: bool,
    username: Option<String>,
) -> Result<PreparedConnection, ConnectionError> {
    let cancellation = connection.cancellation(&binding_id)?;
    let normalized = origin.as_deref().map(normalize_origin).transpose()?;
    let (bootstrap, local_proof) = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(stale()),
        result = async {
            let management = if normalized.is_none() { Some(local.bootstrap().await.map_err(|_| unavailable())?) } else { None };
            let origin = normalized.as_deref().or_else(|| management.as_ref().map(|value| value.base_url.as_str())).ok_or_else(unavailable)?;
            let capabilities = discover(origin).await?;
            let proof = management.as_ref().map(|value| value.access_token.clone());
            let bootstrap = if capabilities.mode == HostMode::Enterprise {
                login(origin, HostLoginRequest::Enterprise { username: username.unwrap_or_default(), password: SecretValue::new(password.clone()), native: true }).await?
            } else if let Some(management) = management { management }
            else { remote_login(origin, &password).await? };
            Ok::<_, ConnectionError>((bootstrap, proof))
        } => result?,
    };
    connection.bind_with_proof(
        &binding_id,
        bootstrap.clone(),
        normalized.is_none(),
        local_proof,
    )?;
    let warning = if let Some(origin) =
        normalized.filter(|_| bootstrap.capabilities.mode == HostMode::Personal)
    {
        passwords::save(app.config().identifier.clone(), origin, password, remember)
            .await
            .err()
            .map(|_| "已连接，但系统钥匙串不可用，未能更新记住的密码。")
    } else {
        None
    };
    connection.cancellation(&binding_id)?;
    Ok(PreparedConnection { bootstrap, warning })
}

#[tauri::command]
pub(crate) async fn refresh_runtime_connection(
    browsers: State<'_, crate::browser_resource::BrowserResourceManager>,
    connection: State<'_, RuntimeConnection>,
    local: State<'_, RuntimeBootstrapCoordinator>,
    binding_id: String,
) -> Result<RuntimeBootstrap, ConnectionError> {
    let target = connection.target(&binding_id)?;
    let bootstrap = tokio::select! {
        biased;
        () = target.cancellation.cancelled() => return Err(stale()),
        result = async {
            if target.context.local && target.context.bootstrap.capabilities.mode == HostMode::Personal {
                let bootstrap = local.bootstrap().await.map_err(|_| unavailable())?;
                if bootstrap.capabilities.mode != HostMode::Personal { return Err(failure("authentication_required", "Host 模式已变化，请重新登录。")); }
                Ok(bootstrap)
            }
            else { verify_remote(&target.context.bootstrap.base_url, &target.context.bootstrap.access_token).await }
        } => result?,
    };
    connection.bind_with_proof(
        &binding_id,
        bootstrap.clone(),
        target.context.local,
        target.context.local_proof.clone(),
    )?;
    browsers.close_cancelled();
    Ok(bootstrap)
}

#[tauri::command]
pub(crate) async fn probe_runtime_target(
    origin: String,
) -> Result<RuntimeHostCapabilities, ConnectionError> {
    discover(&normalize_origin(&origin)?).await
}
async fn discover(origin: &str) -> Result<RuntimeHostCapabilities, ConnectionError> {
    let response = http()?
        .get(format!("{origin}/capabilities"))
        .headers(crate::runtime_compatibility::headers())
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    decode_json(response).await
}

#[derive(Serialize)]
pub(crate) struct RestoredConnection {
    #[serde(flatten)]
    bootstrap: RuntimeBootstrap,
    binding_id: String,
    target_kind: &'static str,
}
/// WebView 刷新复用进程内的普通 Token；应用退出即消失，不从磁盘恢复企业登录。
#[tauri::command]
pub(crate) fn restore_runtime_connection(
    connection: State<'_, RuntimeConnection>,
) -> Result<Option<RestoredConnection>, ConnectionError> {
    let selected = connection.selected.lock().map_err(|_| unavailable())?;
    Ok(selected.as_ref().and_then(|value| {
        value.context.as_ref().map(|context| RestoredConnection {
            bootstrap: context.bootstrap.clone(),
            binding_id: value.id.clone(),
            target_kind: if context.local { "local" } else { "remote" },
        })
    }))
}

#[tauri::command]
pub(crate) async fn remembered_runtime_password(
    app: AppHandle,
    origin: String,
) -> Result<Option<String>, ConnectionError> {
    let origin = normalize_origin(&origin)?;
    passwords::read(app.config().identifier.clone(), origin)
        .await
        .map_err(|_| failure("keychain_unavailable", "系统钥匙串不可用，请手动输入密码。"))
}

#[tauri::command]
pub(crate) async fn open_runtime_web(
    app: AppHandle,
    target: RuntimeTarget,
) -> Result<(), ConnectionError> {
    let bootstrap = target.bootstrap().await?;
    let result = tokio::select! {
        biased;
        () = target.cancellation.cancelled() => return Err(stale()),
        result = http()?.post(format!("{}/auth/login", bootstrap.base_url)).bearer_auth(&bootstrap.access_token).headers(crate::runtime_compatibility::headers()).json(&HostLoginRequest::Desktop).send() => result.map_err(|_| unavailable())?,
    };
    let login = decode_login(result).await?;
    target.ensure_active()?;
    let token = login.token.ok_or_else(unavailable)?;
    let mut url = url::Url::parse(&bootstrap.base_url).map_err(|_| unavailable())?;
    url.set_fragment(Some(&format!("token={}", token.expose())));
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|_| failure("browser_open_failed", "无法打开系统浏览器。"))
}

fn normalize_origin(input: &str) -> Result<String, ConnectionError> {
    let invalid = || {
        failure(
            "invalid_runtime_address",
            "请输入 http:// 或 https:// 开头的 Host 地址，不包含路径、账号或参数。",
        )
    };
    let url = url::Url::parse(input.trim()).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(invalid());
    }
    Ok(url.origin().ascii_serialization())
}
fn http() -> Result<reqwest::Client, ConnectionError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| unavailable())
}
async fn decode_login(response: reqwest::Response) -> Result<HostLoginResult, ConnectionError> {
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(failure("authentication_required", "密码错误或登录已失效。"));
    }
    if response.status().is_redirection() {
        return Err(ConnectionError {
            code: "runtime_unavailable",
            message: format!(
                "Host 返回 HTTP {}，已拒绝转发登录凭据。",
                response.status().as_u16()
            ),
        });
    }
    if !response.status().is_success() {
        let value: serde_json::Value = decode_json(response).await?;
        let (code, message) = match value["error"]["code"].as_str() {
            Some("center_unavailable") => ("center_unavailable", "企业中心暂不可达，请稍后重试。"),
            Some("center_identity_mismatch") => (
                "center_identity_mismatch",
                "企业中心身份与本机绑定不一致，请由本机管理者检查中心配置。",
            ),
            Some("center_protocol_incompatible") => (
                "center_protocol_incompatible",
                "企业中心协议不兼容，请更新服务。",
            ),
            Some("client_too_old" | "host_too_old" | "component_mismatch") => (
                "component_mismatch",
                "Host 与 Desktop 版本不兼容，请更新对应应用。",
            ),
            _ => (
                "runtime_unavailable",
                "Host 暂时无法完成请求，请检查访问设置或稍后重试。",
            ),
        };
        return Err(failure(code, message));
    }
    decode_json(response).await
}
// Login and capabilities are small control-plane responses, including before trusting a Host.
async fn decode_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, ConnectionError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 64 * 1024 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "component_mismatch",
            "Host 响应格式不匹配，请核对 Host 与 Desktop 版本。",
        )
    })
}

async fn remote_login(origin: &str, password: &str) -> Result<RuntimeBootstrap, ConnectionError> {
    login(
        origin,
        HostLoginRequest::Password {
            password: SecretValue::new(password.to_owned()),
            native: true,
        },
    )
    .await
}
async fn login(
    origin: &str,
    request: HostLoginRequest,
) -> Result<RuntimeBootstrap, ConnectionError> {
    let login = decode_login(
        http()?
            .post(format!("{origin}/auth/login"))
            .headers(crate::runtime_compatibility::headers())
            .json(&request)
            .send()
            .await
            .map_err(|error| transport_error(&error).at_step("发送登录请求"))?,
    )
    .await
    .map_err(|error| error.at_step("读取登录响应"))?;
    let token = login
        .token
        .ok_or_else(|| failure("component_mismatch", "Host 未返回原生登录凭据。"))?;
    verify_remote(origin, token.expose()).await
}
async fn verify_remote(origin: &str, token: &str) -> Result<RuntimeBootstrap, ConnectionError> {
    verify_remote_with_client(&http()?, origin, token).await
}

async fn verify_remote_with_client(
    http: &reqwest::Client,
    origin: &str,
    token: &str,
) -> Result<RuntimeBootstrap, ConnectionError> {
    let session = decode_login(
        http.get(format!("{origin}/auth/session"))
            .bearer_auth(token)
            .headers(crate::runtime_compatibility::headers())
            .send()
            .await
            .map_err(|error| transport_error(&error).at_step("查询登录状态"))?,
    )
    .await
    .map_err(|error| error.at_step("验证登录状态"))?;
    let response = http
        .get(format!("{origin}/capabilities"))
        .bearer_auth(token)
        .headers(crate::runtime_compatibility::headers())
        .send()
        .await
        .map_err(|error| transport_error(&error).at_step("查询 Host 能力"))?;
    if !response.status().is_success() {
        return Err(ConnectionError {
            code: "runtime_unavailable",
            message: format!("查询 Host 能力返回 HTTP {}。", response.status().as_u16()),
        });
    }
    let capabilities: RuntimeHostCapabilities = decode_json(response).await.map_err(|_| {
        failure(
            "component_mismatch",
            "Host 缺少有效的软件版本声明，请更新 Host。",
        )
    })?;
    if assistant_protocol::check_compatibility(
        Some(&assistant_protocol::ClientCompatibility::current()),
        &assistant_protocol::ClientCompatibility {
            version: capabilities.runtime_version.clone(),
            min_compatible_version: capabilities.min_compatible_version.clone(),
        },
    )
    .is_err()
        || !capabilities.sse
        || !capabilities
            .features
            .contains(&RuntimeHostFeature::WebLogin)
    {
        return Err(failure(
            "component_mismatch",
            "Host 与 Desktop 软件版本不兼容或缺少所需能力，请更新对应应用。",
        ));
    }
    Ok(RuntimeBootstrap {
        base_url: origin.to_owned(),
        instance_id: session.instance_id,
        access_token: token.to_owned(),
        capabilities,
        started_runtime: false,
    })
}

/// Host 管理始终走本机 bootstrap，与当前选择的企业账号及远程目标无关。
#[tauri::command]
pub(crate) async fn manage_local_host(
    local: State<'_, RuntimeBootstrapCoordinator>,
    command: assistant_protocol::HostAccessCommand,
) -> Result<assistant_protocol::HostAccessStatus, ConnectionError> {
    let bootstrap = local.bootstrap().await.map_err(|_| unavailable())?;
    let response = http()?.post(format!("{}/commands", bootstrap.base_url))
        .bearer_auth(&bootstrap.access_token).headers(crate::runtime_compatibility::headers())
        .json(&serde_json::json!({"request_id":"desktop-host-management","command":{"scope":"host_access","payload":command}}))
        .send().await.map_err(|_| unavailable())?;
    let success = response.status().is_success();
    let value: serde_json::Value = decode_json(response).await?;
    if !success {
        return Err(ConnectionError {
            code: "configuration_unavailable",
            message: value["error"]["message"]
                .as_str()
                .unwrap_or("无法保存 Host 配置。")
                .to_owned(),
        });
    }
    serde_json::from_value(value["result"]["payload"].clone()).map_err(|_| unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap(origin: &str, secret: &str) -> RuntimeBootstrap {
        RuntimeBootstrap {
            base_url: origin.into(),
            instance_id: origin.into(),
            access_token: secret.into(),
            started_runtime: false,
            capabilities: RuntimeHostCapabilities {
                mode: assistant_protocol::HostMode::Personal,
                platform: None,
                architecture: None,
                rg_on_path: None,
                min_compatible_version: assistant_protocol::MIN_COMPATIBLE_VERSION.into(),
                runtime_version: env!("CARGO_PKG_VERSION").into(),
                max_command_bytes: 1048576,
                max_attachment_bytes: None,
                sse: true,
                streaming_upload: true,
                features: vec![RuntimeHostFeature::WebLogin],
            },
        }
    }

    #[tokio::test]
    async fn local_enterprise_user_token_and_native_proof_stay_separate() {
        let connection = RuntimeConnection::default();
        let id = connection.begin().unwrap();
        let mut user = bootstrap("http://127.0.0.1:1234", "ordinary-user-token");
        user.capabilities.mode = HostMode::Enterprise;
        connection
            .bind_with_proof(&id, user, true, Some("local-management-proof".into()))
            .unwrap();
        let target = connection.target(&id).unwrap();
        assert_eq!(
            target.bootstrap().await.unwrap().access_token,
            "ordinary-user-token"
        );
        assert_eq!(
            target.native_headers()["x-ez-host-bootstrap"],
            "local-management-proof"
        );
        let next = connection.begin().unwrap();
        connection
            .bind(
                &next,
                bootstrap("http://remote.example", "remote-token"),
                false,
            )
            .unwrap();
        assert!(target.ensure_active().is_err());
        assert!(
            !connection
                .target(&next)
                .unwrap()
                .native_headers()
                .contains_key("x-ez-host-bootstrap")
        );
    }

    #[test]
    fn origins_allow_explicit_http_and_https_but_reject_credentials_paths_and_parameters() {
        assert_eq!(
            normalize_origin(" HTTP://EXAMPLE.COM:80/ ").unwrap(),
            "http://example.com"
        );
        assert_eq!(
            normalize_origin("https://[::1]:8443").unwrap(),
            "https://[::1]:8443"
        );
        for invalid in [
            "192.168.1.20:8080",
            "file:///tmp",
            "http://user:secret@host",
            "https://host/api",
            "https://host/?token=secret",
            "https://host/#secret",
        ] {
            assert!(normalize_origin(invalid).is_err(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn a_b_a_selection_revokes_old_leases_and_never_retargets_them() {
        let connection = RuntimeConnection::default();
        let first = connection.begin().unwrap();
        connection
            .bind(&first, bootstrap("http://127.0.0.1:1234", "a"), true)
            .unwrap();
        let old = connection.target(&first).unwrap();
        let old_upload = old.cancellation.child_token();
        let second = connection.begin().unwrap();
        connection
            .bind(&second, bootstrap("http://192.168.1.20:1234", "b"), false)
            .unwrap();
        assert!(old_upload.is_cancelled());
        assert!(old.bootstrap().await.is_err());
        assert!(connection.target(&first).is_err());
        let remote = connection.target(&second).unwrap();
        assert!(remote.ensure_local().is_err());
        assert_eq!(remote.bootstrap().await.unwrap().access_token, "b");
        let third = connection.begin().unwrap();
        connection
            .bind(&third, bootstrap("http://127.0.0.1:1234", "a-new"), true)
            .unwrap();
        assert_ne!(first, third);
        assert!(
            connection
                .bind(&first, bootstrap("http://wrong-host", "old"), false)
                .is_err()
        );
        assert!(remote.bootstrap().await.is_err());
        assert!(connection.target(&third).unwrap().ensure_local().is_ok());
    }

    #[tokio::test]
    async fn refreshing_a_binding_revokes_its_previous_resource_context() {
        let connection = RuntimeConnection::default();
        let id = connection.begin().unwrap();
        connection
            .bind(&id, bootstrap("http://127.0.0.1:1234", "old"), true)
            .unwrap();
        let old = connection.target(&id).unwrap();
        assert!(old.same_connection(&connection.target(&id).unwrap()));
        connection
            .bind(&id, bootstrap("http://127.0.0.1:1234", "new"), true)
            .unwrap();
        let new = connection.target(&id).unwrap();
        assert!(old.ensure_active().is_err());
        assert!(!old.same_connection(&new));
        assert!(new.ensure_active().is_ok());
        assert!(!connection.cancellation(&id).unwrap().is_cancelled());
    }

    #[tokio::test]
    async fn native_http_client_does_not_follow_a_redirect_or_forward_credentials() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = source.local_addr().unwrap();
        let location = destination.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut request = [0u8; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{location}/auth/login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let error = remote_login(&format!("http://{address}"), "test-password")
            .await
            .map(|_| ())
            .expect_err("redirect must be rejected");
        assert_eq!(error.code, "runtime_unavailable");
        assert!(error.message.contains("读取登录响应"));
        assert!(error.message.contains("307"));
        assert!(!error.message.contains("test-password"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), destination.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_login_response_is_distinct_from_network_failure_and_redacted() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = source.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut request = [0u8; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let body = r#"{"token":"sensitive-response-token"}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let error = remote_login(&format!("http://{address}"), "test-password")
            .await
            .map(|_| ())
            .expect_err("incomplete login response must fail");
        assert_eq!(error.code, "component_mismatch");
        assert!(error.message.contains("读取登录响应"));
        assert!(error.message.contains("响应格式不匹配"));
        assert!(!error.message.contains("sensitive-response-token"));
        assert!(!error.message.contains("test-password"));
        server.await.unwrap();
    }
    #[tokio::test]
    #[ignore = "显式运行的双 Host HTTPS 验证；测试 CA 仅附加到本测试 Client，系统信任不变"]
    async fn isolated_https_host_rejects_untrusted_and_accepts_explicit_test_ca() {
        let origin =
            std::env::var("EZ_ASSISTANT_TEST_HTTPS_ORIGIN").expect("isolated HTTPS origin");
        let certificate = std::env::var("EZ_ASSISTANT_TEST_HTTPS_CA").expect("isolated test CA");
        assert!(origin.starts_with("https://127.0.0.1:"));
        assert!(remote_login(&origin, "Dev-remote-025").await.is_err());
        let trusted = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .add_root_certificate(
                reqwest::Certificate::from_pem(&std::fs::read(certificate).unwrap()).unwrap(),
            )
            .build()
            .unwrap();
        let response = trusted
            .post(format!("{origin}/auth/login"))
            .headers(crate::runtime_compatibility::headers())
            .json(&HostLoginRequest::Password {
                password: SecretValue::new("Dev-remote-025".into()),
                native: true,
            })
            .send()
            .await
            .unwrap();
        let login = decode_login(response).await.unwrap();
        let prepared = verify_remote_with_client(&trusted, &origin, login.token.unwrap().expose())
            .await
            .unwrap();
        assert_eq!(prepared.base_url, origin);
        assert_eq!(prepared.instance_id, login.instance_id);
        assert_eq!(
            prepared.capabilities.min_compatible_version,
            assistant_protocol::MIN_COMPATIBLE_VERSION
        );
    }
}
