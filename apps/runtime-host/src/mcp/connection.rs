//! `rmcp` 客户端连接、受控 stdio 子进程与 Streamable HTTP Transport。

#[cfg(test)]
mod handshake_tests;
mod startup_diagnostic;
#[cfg(windows)]
mod windows;

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context, Poll},
};

use assistant_runtime::{
    McpConnection, McpConnectionError, McpConnectionFactory, McpConnectionFailureKind,
    McpConnectionFuture, McpConnectionOptions, McpRawCallResult, McpRawContent, McpSecret,
    McpServerConfig, McpServerTransportConfig, McpToolDefinition, McpToolPage,
};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use reqwest::{
    Client,
    header::{HeaderName, HeaderValue},
    redirect::Policy,
};
use rmcp::{
    ClientCacheConfig, ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, ContentBlock, PaginatedRequestParams,
        ProtocolVersion, ResourceContents,
    },
    service::{RunningService, RxJsonRpcMessage, ServiceError, TxJsonRpcMessage},
    transport::{
        Transport,
        async_rw::AsyncRwTransport,
        common::client_side_sse::NeverRetry,
        streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        },
    },
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    process::{ChildStderr, ChildStdin, ChildStdout, Command},
    sync::RwLock,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::cleanup::McpProcessCleanup;

use crate::config_source::prepare_private_directory;

pub(crate) const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MCP_WORK_DIRECTORY: &str = "mcp";
const INHERITED_ENVIRONMENT: &[&str] = &[
    "PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

type ClientService = RunningService<RoleClient, ()>;

pub(crate) struct HostMcpConnectionFactory {
    runtime_home: PathBuf,
    cleanup: Arc<McpProcessCleanup>,
}

impl HostMcpConnectionFactory {
    pub(crate) fn new(runtime_home: PathBuf) -> Self {
        Self {
            runtime_home,
            cleanup: Arc::new(McpProcessCleanup::default()),
        }
    }

    /// Runtime 已停止并释放 MCP 连接后，Host 必须等待取消/Drop 路径的进程回收。
    pub(crate) async fn shutdown(&self) -> io::Result<()> {
        self.cleanup.shutdown().await
    }
}

impl McpConnectionFactory for HostMcpConnectionFactory {
    fn connect(
        &self,
        server: McpServerConfig,
        options: McpConnectionOptions,
        cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, Arc<dyn McpConnection>> {
        let runtime_home = self.runtime_home.clone();
        let cleanup = self.cleanup.clone();
        Box::pin(async move {
            let service = match server.transport() {
                McpServerTransportConfig::Stdio { .. } => {
                    connect_stdio(&runtime_home, &server, cancellation.clone(), cleanup).await
                }
                McpServerTransportConfig::StreamableHttp { .. } => {
                    connect_http(&server, options, cancellation.clone()).await
                }
            }
            .map_err(|error| startup_diagnostic::with_source(server.source(), error))?;
            service
                .peer()
                .set_response_cache_config(ClientCacheConfig::disabled())
                .await;
            Ok(Arc::new(HostMcpConnection {
                service: RwLock::new(Some(service)),
            }) as Arc<dyn McpConnection>)
        })
    }
}

struct HostMcpConnection {
    service: RwLock<Option<ClientService>>,
}

impl McpConnection for HostMcpConnection {
    fn list_tools_page(
        &self,
        cursor: Option<String>,
        cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, McpToolPage> {
        Box::pin(async move {
            let service = self.service.read().await;
            let Some(service) = service.as_ref() else {
                return Err(connection_error(
                    McpConnectionFailureKind::Catalog,
                    "MCP 连接已关闭",
                ));
            };
            let params =
                cursor.map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let result = tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(cancelled_error());
                }
                result = service.list_tools(params) => result,
            }
            .map_err(|_| {
                connection_error(
                    McpConnectionFailureKind::Catalog,
                    "读取 MCP 工具列表失败，请检查服务是否正常运行",
                )
            })?;
            let encoded_size = serde_json::to_vec(&result)
                .map_err(|_| {
                    connection_error(
                        McpConnectionFailureKind::Catalog,
                        "MCP 工具列表返回格式无效",
                    )
                })?
                .len();
            if encoded_size > MAX_MESSAGE_BYTES {
                return Err(connection_error(
                    McpConnectionFailureKind::Catalog,
                    "MCP 工具列表超过大小限制",
                ));
            }
            Ok(McpToolPage {
                tools: result.tools.into_iter().map(convert_tool).collect(),
                next_cursor: result.next_cursor,
            })
        })
    }

    fn call_tool_once(
        &self,
        tool_name: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, McpRawCallResult> {
        Box::pin(async move {
            let service = self.service.read().await;
            let Some(service) = service.as_ref() else {
                return Err(connection_error(
                    McpConnectionFailureKind::Connect,
                    "MCP 连接已关闭",
                ));
            };
            let request = CallToolRequestParams::new(tool_name).with_arguments(arguments);
            let response = tokio::select! {
                () = cancellation.cancelled() => return Err(cancelled_error()),
                response = service.call_tool_once(request) => response,
            }
            .map_err(tool_request_error)?;
            let result = match response {
                CallToolResponse::Complete(result) => result,
                CallToolResponse::InputRequired(_) | CallToolResponse::Task(_) => {
                    return Err(connection_error(
                        McpConnectionFailureKind::UnsupportedExtension,
                        "MCP 工具要求继续交互，当前版本暂不支持",
                    ));
                }
                _ => {
                    return Err(connection_error(
                        McpConnectionFailureKind::UnsupportedExtension,
                        "MCP 工具返回了当前版本不支持的响应",
                    ));
                }
            };
            let encoded_size = serde_json::to_vec(&result)
                .map_err(|_| {
                    connection_error(McpConnectionFailureKind::ToolCall, "MCP 工具结果格式无效")
                })?
                .len();
            if encoded_size > MAX_MESSAGE_BYTES {
                return Err(connection_error(
                    McpConnectionFailureKind::ToolCall,
                    "MCP 工具结果超过大小限制",
                ));
            }
            Ok(convert_call_result(result))
        })
    }

    fn close(&self, cancellation: CancellationToken) -> McpConnectionFuture<'_, ()> {
        Box::pin(async move {
            let service = self.service.write().await.take();
            let Some(mut service) = service else {
                return Ok(());
            };
            tokio::select! {
                () = cancellation.cancelled() => {
                    service.cancellation_token().cancel();
                    service.close().await.map_err(|_| connection_error(
                        McpConnectionFailureKind::Close,
                        "MCP 连接清理失败",
                    ))?;
                }
                result = service.close() => {
                    result.map_err(|_| connection_error(
                        McpConnectionFailureKind::Close,
                        "MCP 连接清理失败",
                    ))?;
                }
            }
            Ok(())
        })
    }
}

async fn connect_stdio(
    runtime_home: &Path,
    server: &McpServerConfig,
    cancellation: CancellationToken,
    cleanup: Arc<McpProcessCleanup>,
) -> Result<ClientService, McpConnectionError> {
    let McpServerTransportConfig::Stdio {
        command: program,
        args,
        cwd: configured_cwd,
        environment,
    } = server.transport()
    else {
        return Err(connection_error(
            McpConnectionFailureKind::InvalidConfig,
            "MCP 本地进程配置不可用，请检查 command 和 args",
        ));
    };
    let cwd = resolve_working_directory(runtime_home, server, configured_cwd.as_deref())?;
    let environment = resolve_secret_map(
        environment,
        matches!(server.source(), assistant_protocol::McpServerSource::User),
    )?;
    #[cfg(windows)]
    let program = windows::resolve_program(program, &cwd, &environment)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in INHERITED_ENVIRONMENT {
        if server.removed_environment().iter().any(|removed| {
            if cfg!(windows) {
                removed.eq_ignore_ascii_case(name)
            } else {
                removed == name
            }
        }) {
            continue;
        }
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let configured_path = environment
        .iter()
        .find(|(name, _)| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case("PATH")
            } else {
                name.as_str() == "PATH"
            }
        })
        .map(|(_, value)| std::ffi::OsStr::new(value));
    let inherited_path = if server.removed_environment().iter().any(|name| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case("PATH")
        } else {
            name == "PATH"
        }
    }) {
        None
    } else {
        std::env::var_os("PATH")
    };
    if let Some(path) = crate::runtime_tools::prepend_path(
        configured_path.or(inherited_path.as_deref()),
    )
    .map_err(|_| {
        connection_error(
            McpConnectionFailureKind::InvalidConfig,
            "内置工具的 PATH 配置无效",
        )
    })? {
        command.env("PATH", path);
    } else if let Some(path) = configured_path {
        command.env("PATH", path);
    }
    command.envs(environment.iter().filter(|(name, _)| {
        if cfg!(windows) {
            !name.eq_ignore_ascii_case("PATH")
        } else {
            name.as_str() != "PATH"
        }
    }));

    for name in server.removed_environment() {
        command.env_remove(name);
    }

    let mut child = spawn_managed(command).map_err(|error| {
        connection_error(
            McpConnectionFailureKind::Connect,
            match error.kind() {
                io::ErrorKind::NotFound => {
                    "找不到 MCP 启动程序或解释器，请检查 command 和 PATH 配置"
                }
                io::ErrorKind::PermissionDenied => "没有权限执行 MCP 启动程序，请检查文件执行权限",
                io::ErrorKind::InvalidInput => "MCP 启动参数无效，请检查 args 配置",
                _ => "MCP 进程启动失败，请检查启动程序和运行环境",
            },
        )
    })?;
    let stdin = child.stdin().take().ok_or_else(|| {
        connection_error(
            McpConnectionFailureKind::Connect,
            "无法建立 MCP 标准输入管道",
        )
    })?;
    let stdout = child.stdout().take().ok_or_else(|| {
        connection_error(
            McpConnectionFailureKind::Connect,
            "无法建立 MCP 标准输出管道",
        )
    })?;
    let stderr = child.stderr().take().ok_or_else(|| {
        connection_error(
            McpConnectionFailureKind::Connect,
            "无法建立 MCP 诊断输出管道",
        )
    })?;
    let transport = ManagedStdioTransport::new(stdout, stdin, stderr, child, cleanup);
    let diagnostic = transport.startup_diagnostic.clone();
    serve(transport, cancellation).await.map_err(|error| {
        if error.kind() == McpConnectionFailureKind::Protocol
            && let Some(message) = diagnostic.borrow().as_ref()
        {
            return connection_error(McpConnectionFailureKind::InvalidConfig, message);
        }
        error
    })
}

async fn connect_http(
    server: &McpServerConfig,
    options: McpConnectionOptions,
    cancellation: CancellationToken,
) -> Result<ClientService, McpConnectionError> {
    let McpServerTransportConfig::StreamableHttp {
        url,
        headers: configured_headers,
    } = server.transport()
    else {
        return Err(connection_error(
            McpConnectionFailureKind::InvalidConfig,
            "MCP HTTP 配置不可用，请检查服务地址",
        ));
    };
    let resolved = resolve_secret_map(
        configured_headers,
        matches!(server.source(), assistant_protocol::McpServerSource::User),
    )?;
    let mut headers = HashMap::new();
    for (name, value) in resolved {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            connection_error(
                McpConnectionFailureKind::InvalidConfig,
                "MCP HTTP 请求头名称无效",
            )
        })?;
        let value = HeaderValue::from_str(&value).map_err(|_| {
            connection_error(
                McpConnectionFailureKind::InvalidConfig,
                "MCP HTTP 请求头值无效",
            )
        })?;
        headers.insert(name, value);
    }
    let client = Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|_| {
            connection_error(McpConnectionFailureKind::Connect, "无法创建 MCP HTTP 连接")
        })?;
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned())
        .max_concurrent_requests(options.max_concurrent_requests)
        .control_request_timeout(options.control_timeout)
        .session_recovery_timeout(options.control_timeout)
        .custom_headers(headers)
        .max_sse_event_size(MAX_MESSAGE_BYTES)
        .reinit_on_expired_session(false);
    config.retry_config = Arc::new(NeverRetry::default());
    let transport = StreamableHttpClientTransport::with_client(
        super::http_client::BoundedHttpClient(client),
        config,
    );
    serve(transport, cancellation).await
}

async fn serve<T, E, A>(
    transport: T,
    cancellation: CancellationToken,
) -> Result<ClientService, McpConnectionError>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    tokio::select! {
        () = cancellation.cancelled() => Err(cancelled_error()),
        result = ().serve_with_lifecycle(transport, lifecycle()) => result.map_err(|_| {
            connection_error(
                McpConnectionFailureKind::Protocol,
                "MCP 连接初始化失败：服务未完成协议握手，请检查服务配置、启动环境及协议兼容性",
            )
        }),
    }
}

fn lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: Some(ProtocolVersion::V_2025_11_25),
    }
}

fn resolve_working_directory(
    runtime_home: &Path,
    server: &McpServerConfig,
    configured: Option<&str>,
) -> Result<PathBuf, McpConnectionError> {
    if let Some(configured) = configured {
        let path = PathBuf::from(configured);
        if !path.is_absolute() || !path.is_dir() {
            return Err(connection_error(
                McpConnectionFailureKind::InvalidConfig,
                "MCP 工作目录不可用，请检查 cwd 是否为已存在的绝对路径",
            ));
        }
        return Ok(path);
    }
    let path = runtime_home
        .join(MCP_WORK_DIRECTORY)
        .join(server.server_key().as_str());
    prepare_private_directory(&path).map_err(|_| {
        connection_error(
            McpConnectionFailureKind::Connect,
            "无法创建 MCP 工作目录，请检查目录权限",
        )
    })?;
    Ok(path)
}

fn resolve_secret_map(
    configured: &BTreeMap<String, McpSecret>,
    allow_environment_reference: bool,
) -> Result<BTreeMap<String, String>, McpConnectionError> {
    configured
        .iter()
        .map(|(name, value)| {
            // 受管参数已经过来源解析，不再把用户值解释为宿主环境引用。
            (if allow_environment_reference {
                resolve_secret(value)
            } else {
                Some(value.expose().to_owned())
            })
            .map(|value| (name.clone(), value))
            .ok_or_else(|| {
                McpConnectionError::new(
                    McpConnectionFailureKind::InvalidConfig,
                    format!("配置项 {name} 引用的环境变量未设置或无效，请检查 env 或 headers 配置"),
                )
            })
        })
        .collect()
}

fn resolve_secret(value: &McpSecret) -> Option<String> {
    let value = value.expose();
    let Some(name) = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    else {
        return Some(value.to_owned());
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    std::env::var_os(name)?.into_string().ok()
}

fn convert_tool(tool: rmcp::model::Tool) -> McpToolDefinition {
    McpToolDefinition {
        name: tool.name.into_owned(),
        title: tool.title,
        description: tool.description.map(Cow::into_owned),
        input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
        output_schema: tool
            .output_schema
            .map(|schema| serde_json::Value::Object((*schema).clone())),
        annotations: tool
            .annotations
            .and_then(|annotations| serde_json::to_value(annotations).ok()),
    }
}

fn convert_call_result(result: rmcp::model::CallToolResult) -> McpRawCallResult {
    McpRawCallResult {
        content: result.content.into_iter().map(convert_content).collect(),
        structured_content: result.structured_content,
        is_error: result.is_error.unwrap_or(false),
    }
}

fn convert_content(content: ContentBlock) -> McpRawContent {
    match content {
        ContentBlock::Text(content) => McpRawContent::Text { text: content.text },
        ContentBlock::Image(content) => McpRawContent::Image {
            data_base64: content.data,
            media_type: content.mime_type,
        },
        ContentBlock::Audio(_) => McpRawContent::Audio,
        ContentBlock::ResourceLink(resource) => McpRawContent::ResourceLink {
            uri: resource.uri,
            name: resource.name,
            title: resource.title,
            description: resource.description,
            media_type: resource.mime_type,
            size: resource.size,
        },
        ContentBlock::Resource(resource) => match resource.resource {
            ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                ..
            } => McpRawContent::EmbeddedText {
                uri,
                media_type: mime_type,
                text,
            },
            ResourceContents::BlobResourceContents { .. } => McpRawContent::EmbeddedBlob,
            _ => McpRawContent::Unsupported,
        },
        _ => McpRawContent::Unsupported,
    }
}

fn spawn_managed(command: Command) -> io::Result<Box<dyn ChildWrapper>> {
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    command.spawn_with(|command| {
        // JobObject 会重设创建标志；在最终启动边界隐藏 MCP 控制台，
        // 并保留先加入 Job 再恢复线程所需的挂起位，与本地 Shell 启动一致。
        #[cfg(windows)]
        command.creation_flags(0x0800_0000 | 0x0000_0004); // CREATE_NO_WINDOW | CREATE_SUSPENDED
        command.spawn()
    })
}

struct ManagedStdioTransport {
    inner: AsyncRwTransport<RoleClient, BoundedLineReader<ChildStdout>, ChildStdin>,
    child: Option<Box<dyn ChildWrapper>>,
    stderr_task: Option<JoinHandle<()>>,
    startup_diagnostic: tokio::sync::watch::Receiver<Option<&'static str>>,
    cleanup: Arc<McpProcessCleanup>,
    // Transport 尚未 Drop 时也占有 Tracker token，防止 Host 在回收任务登记前误判全部完成。
    _cleanup_lease: tokio_util::task::task_tracker::TaskTrackerToken,
}

impl ManagedStdioTransport {
    fn new(
        stdout: ChildStdout,
        stdin: ChildStdin,
        stderr: ChildStderr,
        child: Box<dyn ChildWrapper>,
        cleanup: Arc<McpProcessCleanup>,
    ) -> Self {
        let (diagnostic, startup_diagnostic) = tokio::sync::watch::channel(None);
        let stderr_task = tokio::spawn(startup_diagnostic::drain(stderr, diagnostic));
        Self {
            inner: AsyncRwTransport::new_client(BoundedLineReader::new(stdout), stdin),
            child: Some(child),
            stderr_task: Some(stderr_task),
            startup_diagnostic,
            _cleanup_lease: cleanup.lease(),
            cleanup,
        }
    }
}

impl Transport<RoleClient> for ManagedStdioTransport {
    type Error = io::Error;

    fn name() -> Cow<'static, str> {
        "managed-stdio".into()
    }

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        let message = self.inner.receive().await;
        if message.is_none()
            && let Some(task) = self.stderr_task.as_mut()
        {
            // stdout EOF 常早于 stderr reader 获得调度；有界排空后再交付握手失败。
            // 未完成的任务仍由原 Transport/cleanup owner 负责，取消不会脱离任务树。
            if let Ok(result) =
                tokio::time::timeout(std::time::Duration::from_millis(100), task).await
            {
                self.stderr_task.take();
                if result.is_err() {
                    // 不记录可能含 secret 的 panic payload；EOF 仍作为连接失败交付。
                    eprintln!("runtime-host: MCP diagnostic reader failed");
                }
            }
        }
        message
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let transport_result = self.inner.close().await;
        // wait 完成前保留 child 所有权；close 被取消时由 Drop 交给受跟踪的回收任务。
        let process_result = if let Some(child) = self.child.as_mut() {
            let kill_error = child.start_kill().err();
            match (kill_error, child.wait().await) {
                (_, Ok(_)) => Ok(()),
                (Some(kill), Err(wait)) => Err(io::Error::other(format!(
                    "MCP process termination failed ({kill}); wait failed ({wait})"
                ))),
                (None, Err(wait)) => Err(wait),
            }
        } else {
            Ok(())
        };
        self.child.take();
        if let Some(stderr_task) = self.stderr_task.take() {
            stderr_task.abort();
            if let Err(error) = stderr_task.await
                && !error.is_cancelled()
            {
                return Err(io::Error::other("MCP diagnostic task failed"));
            }
        }
        transport_result?;
        process_result
    }
}

impl Drop for ManagedStdioTransport {
    fn drop(&mut self) {
        self.cleanup
            .enqueue(self.child.take(), self.stderr_task.take());
    }
}

pub(crate) struct BoundedLineReader<R> {
    inner: R,
    current_line_bytes: usize,
    eof_cancellation: Option<CancellationToken>,
}

impl<R> BoundedLineReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            current_line_bytes: 0,
            eof_cancellation: None,
        }
    }
}

impl<R> BoundedLineReader<R> {
    pub(crate) fn cancel_on_eof(mut self, cancellation: CancellationToken) -> Self {
        self.eof_cancellation = Some(cancellation);
        self
    }

    fn cancel_owner(&self) {
        if let Some(cancellation) = &self.eof_cancellation {
            cancellation.cancel();
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLineReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut bytes = [0_u8; 8192];
        let capacity = bytes.len().min(output.remaining());
        let mut temporary = ReadBuf::new(&mut bytes[..capacity]);
        match Pin::new(&mut self.inner).poll_read(context, &mut temporary) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => {
                self.cancel_owner();
                Poll::Ready(Err(error))
            }
            Poll::Ready(Ok(())) => {
                let filled = temporary.filled();
                if filled.is_empty() {
                    self.cancel_owner();
                }
                for byte in filled {
                    if *byte == b'\n' {
                        self.current_line_bytes = 0;
                    } else {
                        self.current_line_bytes = self.current_line_bytes.saturating_add(1);
                        if self.current_line_bytes > MAX_MESSAGE_BYTES {
                            self.cancel_owner();
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "MCP stdio message exceeds the size limit",
                            )));
                        }
                    }
                }
                output.put_slice(filled);
                Poll::Ready(Ok(()))
            }
        }
    }
}

fn tool_request_error(error: ServiceError) -> McpConnectionError {
    // SDK 错误正文可能包含凭据或远端数据；只投影静态分类。
    match error {
        ServiceError::McpError(_) => {
            connection_error(McpConnectionFailureKind::ToolCall, "MCP 服务拒绝了工具请求")
        }
        ServiceError::Cancelled { .. } => cancelled_error(),
        ServiceError::Timeout { .. } => {
            connection_error(McpConnectionFailureKind::ToolCall, "MCP 工具请求超时")
        }
        ServiceError::SubscriptionLagged { .. } => connection_error(
            McpConnectionFailureKind::ToolCall,
            "MCP 工具响应超过缓冲区限制",
        ),
        ServiceError::TransportSend(_) | ServiceError::TransportClosed => {
            connection_error(McpConnectionFailureKind::Connect, "MCP 工具连接已断开")
        }
        ServiceError::InputRequiredRoundsExceeded { .. } => connection_error(
            McpConnectionFailureKind::UnsupportedExtension,
            "MCP 工具要求继续交互，当前版本暂不支持",
        ),
        _ => connection_error(McpConnectionFailureKind::Protocol, "MCP 工具响应不符合协议"),
    }
}

fn connection_error(kind: McpConnectionFailureKind, message: &'static str) -> McpConnectionError {
    McpConnectionError::new(kind, message)
}

fn cancelled_error() -> McpConnectionError {
    connection_error(McpConnectionFailureKind::Cancelled, "MCP 连接操作已取消")
}

#[cfg(test)]
mod tests {
    use crate::mcp::http_client::BoundedHttpClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{
        Router,
        body::{Body, Bytes},
        http::{HeaderMap, Response, StatusCode},
        routing::{any, post},
    };
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn startup_failure_names_plugin_and_missing_configuration_without_secrets() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        let home = tempfile::tempdir_in(root).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py");
        let server = McpServerConfig::from_plugin_descriptor(
            "general-assistant".to_owned(),
            "a".repeat(64),
            assistant_protocol::McpServerKey::new("deepseek-search").unwrap(),
            json!({"command": if cfg!(windows) { "python.exe" } else { "python3" },
                "args": [fixture, "missing-config"]})
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap();
        let factory = HostMcpConnectionFactory::new(home.path().to_owned());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            factory.connect(
                server,
                McpConnectionOptions {
                    max_concurrent_requests: 1,
                    control_timeout: std::time::Duration::from_secs(2),
                },
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        let Err(error) = result else {
            panic!("missing configuration must fail")
        };
        assert_eq!(error.kind(), McpConnectionFailureKind::InvalidConfig);
        assert!(error.message().contains("来源：插件 · general-assistant"));
        assert!(
            error
                .message()
                .contains("缺少插件原生配置文件 deepseek-search.json")
        );
        assert!(error.message().contains("api_key"));
        assert!(!error.message().contains("private-token"));
        factory.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_line() {
        let data = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut reader = BoundedLineReader::new(std::io::Cursor::new(data));
        let mut output = Vec::new();
        let error = tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut output)
            .await
            .expect_err("oversized line must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn stdio_fixture_negotiates_pages_and_reaps_process() {
        let mut service = start_stdio_fixture(None).await;

        let first = service.list_tools(None).await.expect("first tool page");
        assert_eq!(first.tools[0].name, "first_tool");
        let second = service
            .list_tools(Some(
                PaginatedRequestParams::default().with_cursor(first.next_cursor),
            ))
            .await
            .expect("second tool page");
        assert_eq!(second.tools[0].name, "second_tool");
        assert!(second.next_cursor.is_none());

        service.close().await.expect("close stdio fixture");
    }

    #[tokio::test]
    async fn stdio_fixture_falls_back_to_legacy_lifecycle() {
        let mut service = start_stdio_fixture(Some("legacy")).await;
        let tools = service.list_tools(None).await.expect("legacy tool page");
        assert_eq!(tools.tools[0].name, "first_tool");
        service.close().await.expect("close legacy stdio fixture");
    }

    #[tokio::test]
    async fn stdio_fixture_call_is_converted_without_rmcp_types() {
        let service = start_stdio_fixture(None).await;
        let connection = HostMcpConnection {
            service: RwLock::new(Some(service)),
        };
        let result = connection
            .call_tool_once(
                "first_tool".to_owned(),
                serde_json::Map::from_iter([("value".to_owned(), json!(42))]),
                CancellationToken::new(),
            )
            .await
            .expect("call fixture");
        assert!(!result.is_error);
        assert!(matches!(
            result.content.as_slice(),
            [McpRawContent::Text { text }, McpRawContent::ResourceLink { .. }]
                if text == "called:first_tool"
        ));
        assert_eq!(
            result.structured_content,
            Some(json!({"arguments": {"value": 42}}))
        );
        connection
            .close(CancellationToken::new())
            .await
            .expect("close fixture");
    }

    #[tokio::test]
    async fn stdio_tool_rejection_keeps_transport_but_disconnect_is_classified() {
        for (mode, kind) in [
            ("reject-once", McpConnectionFailureKind::ToolCall),
            ("disconnect", McpConnectionFailureKind::Connect),
        ] {
            let connection = HostMcpConnection {
                service: RwLock::new(Some(start_stdio_fixture(Some(mode)).await)),
            };
            let error = connection
                .call_tool_once(
                    "first_tool".into(),
                    serde_json::Map::new(),
                    CancellationToken::new(),
                )
                .await
                .err()
                .expect("first call fails");
            assert_eq!(error.kind(), kind);
            assert!(!error.message().contains("sensitive fixture error"));
            if mode == "reject-once" {
                let result = connection
                    .call_tool_once(
                        "first_tool".into(),
                        serde_json::Map::new(),
                        CancellationToken::new(),
                    )
                    .await
                    .expect("second explicit call succeeds on same transport");
                assert!(!result.is_error);
            }
            connection
                .close(CancellationToken::new())
                .await
                .expect("close fixture");
        }
    }

    #[test]
    fn tool_request_cancellation_and_timeout_preserve_connection_classification() {
        assert_eq!(
            tool_request_error(ServiceError::Cancelled {
                reason: Some("secret".into())
            })
            .kind(),
            McpConnectionFailureKind::Cancelled
        );
        assert_eq!(
            tool_request_error(ServiceError::Timeout {
                timeout: std::time::Duration::from_secs(1)
            })
            .kind(),
            McpConnectionFailureKind::ToolCall
        );
        assert_eq!(
            tool_request_error(ServiceError::UnexpectedResponse).kind(),
            McpConnectionFailureKind::Protocol
        );
    }

    #[test]
    fn managed_environment_values_are_not_reinterpreted_as_host_secret_references() {
        let server = McpServerConfig::from_plugin_descriptor(
            "fixture".to_owned(), "a".repeat(64), assistant_protocol::McpServerKey::new("fixture").unwrap(),
            serde_json::json!({"command":"fixture", "env":{"TOKEN":"${MCP_TEST_LITERAL_REFERENCE}"}}).as_object().unwrap().clone(),
        ).unwrap();
        let McpServerTransportConfig::Stdio { environment, .. } = server.transport() else {
            panic!("stdio")
        };
        let resolved = resolve_secret_map(environment, false).unwrap();
        assert_eq!(resolved["TOKEN"], "${MCP_TEST_LITERAL_REFERENCE}");
    }

    async fn start_stdio_fixture(mode: Option<&str>) -> ClientService {
        let transport = fixture_transport(mode, Arc::new(McpProcessCleanup::default()));
        serve(transport, CancellationToken::new())
            .await
            .expect("negotiate stdio fixture")
    }

    #[tokio::test]
    async fn dropped_stdio_transport_is_reaped_before_factory_shutdown_returns() {
        let cleanup = Arc::new(McpProcessCleanup::default());
        let transport = fixture_transport(None, cleanup.clone());
        let closing = cleanup.shutdown();
        tokio::pin!(closing);
        assert!(futures_util::poll!(&mut closing).is_pending());
        drop(transport);
        closing.await.expect("reap dropped process and stderr task");
    }

    fn fixture_transport(
        mode: Option<&str>,
        cleanup: Arc<McpProcessCleanup>,
    ) -> ManagedStdioTransport {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py");
        // Windows 的 python3 可能是 Store 占位程序；原生 Python 安装提供 python.exe。
        let mut command = Command::new(if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        });
        command.arg(fixture);
        if let Some(mode) = mode {
            command.arg(mode);
        }
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        for name in ["SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = spawn_managed(command).expect("start stdio fixture");
        let stdin = child.stdin().take().expect("fixture stdin");
        let stdout = child.stdout().take().expect("fixture stdout");
        let stderr = child.stderr().take().expect("fixture stderr");
        ManagedStdioTransport::new(stdout, stdin, stderr, child, cleanup)
    }

    #[derive(Clone, Default)]
    struct HttpFixtureState {
        calls: Arc<AtomicUsize>,
    }

    async fn http_fixture(
        axum::extract::State(state): axum::extract::State<HttpFixtureState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response<Body> {
        assert_eq!(
            headers
                .get("x-fixture")
                .and_then(|value| value.to_str().ok()),
            Some("present")
        );
        state.calls.fetch_add(1, Ordering::SeqCst);
        let request: serde_json::Value =
            serde_json::from_slice(&body).expect("fixture request JSON");
        let result = match request["method"].as_str() {
            Some("server/discover") => json!({
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {}},
                "ttlMs": 0,
                "cacheScope": "private"
            }),
            Some("tools/list") => json!({
                "resultType": "complete",
                "ttlMs": 0,
                "cacheScope": "private",
                "tools": []
            }),
            other => panic!("unexpected fixture method: {other:?}"),
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"jsonrpc": "2.0", "id": request["id"], "result": result}).to_string(),
            ))
            .expect("fixture response")
    }

    #[tokio::test]
    async fn streamable_http_fixture_negotiates_and_lists_zero_tools() {
        let state = HttpFixtureState::default();
        let router = Router::new()
            .route("/mcp", post(http_fixture))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move { axum::serve(listener, router).await });

        let client = Client::builder()
            .redirect(Policy::none())
            .build()
            .expect("HTTP client");
        let mut headers = HashMap::new();
        headers.insert(
            HeaderName::from_static("x-fixture"),
            HeaderValue::from_static("present"),
        );
        let mut config =
            StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp"))
                .custom_headers(headers)
                .max_sse_event_size(MAX_MESSAGE_BYTES)
                .reinit_on_expired_session(false);
        config.retry_config = Arc::new(NeverRetry::default());
        let transport =
            StreamableHttpClientTransport::with_client(BoundedHttpClient(client), config);
        let mut service = serve(transport, CancellationToken::new())
            .await
            .expect("negotiate HTTP fixture");
        let tools = service.list_tools(None).await.expect("list HTTP tools");
        assert!(tools.tools.is_empty());
        service.close().await.expect("close HTTP fixture");
        assert_eq!(state.calls.load(Ordering::SeqCst), 2);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn streamable_http_does_not_follow_redirects() {
        let redirected_calls = Arc::new(AtomicUsize::new(0));
        let counter = redirected_calls.clone();
        let router = Router::new()
            .route(
                "/mcp",
                any(|| async {
                    Response::builder()
                        .status(StatusCode::TEMPORARY_REDIRECT)
                        .header("location", "/redirected")
                        .body(Body::empty())
                        .expect("redirect response")
                }),
            )
            .route(
                "/redirected",
                any(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        StatusCode::OK
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect fixture");
        let address = listener.local_addr().expect("redirect fixture address");
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let client = Client::builder()
            .redirect(Policy::none())
            .build()
            .expect("HTTP client");
        let mut config =
            StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp"))
                .reinit_on_expired_session(false);
        config.retry_config = Arc::new(NeverRetry::default());
        let transport =
            StreamableHttpClientTransport::with_client(BoundedHttpClient(client), config);
        assert!(serve(transport, CancellationToken::new()).await.is_err());
        assert_eq!(redirected_calls.load(Ordering::SeqCst), 0);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn cancelled_connection_does_not_begin_protocol_negotiation() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = serve(tokio::io::duplex(64).0, cancellation).await;
        assert!(matches!(
            result,
            Err(error) if error.kind() == McpConnectionFailureKind::Cancelled
        ));
    }
}
