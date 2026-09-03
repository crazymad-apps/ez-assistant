//! MCP 用户配置、活动目录与 Host 连接 Adapter 的 Runtime 边界。
//!
//! Runtime 解释用户级 `mcp.json`，并作为活动目录、Schema 和连接租约的唯一业务
//! owner；具体文件、HTTP、子进程、图片写入和 `rmcp` 类型只存在于 Host。

mod config;
mod discovery;
mod registry;
mod schema;
mod tool;

use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use agent_types::ToolImageReference;
use assistant_protocol::{McpServerKey, McpTransportKind};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub(crate) use config::{McpConfigStore, McpRegistryCandidate};
pub(crate) use discovery::{DiscoverMcpTools, McpDiscoveryAuthorizationFacts, McpRunDisclosure};
#[cfg(test)]
pub(crate) use registry::ResolvedMcpInvocation;
pub(crate) use registry::{McpCatalogServer, McpRegistry, McpRegistryServerProjection};
pub(crate) use tool::{CallMcpTool, McpAuthorizationFacts, McpDisclosureScope, failure_code};

use crate::{
    ConfigSourceFuture, ConfigSourceReplaceFuture, RuntimeError, RuntimeResult,
    config::McpRuntimeConfig,
};

/// 用户级 MCP 配置文件的窄来源；实现负责文件安全与原子替换，不解释 JSON。
pub trait McpConfigSource: Send + Sync {
    fn load(&self) -> ConfigSourceFuture<'_>;

    fn replace(
        &self,
        expected_revision: Option<String>,
        document: String,
    ) -> ConfigSourceReplaceFuture<'_>;
}

/// 不可用的默认来源，使嵌入式 Runtime 保持既有构造契约且不错误开放 MCP 管理能力。
pub(crate) struct MissingMcpConfigSource;

impl McpConfigSource for MissingMcpConfigSource {
    fn load(&self) -> ConfigSourceFuture<'_> {
        Box::pin(std::future::ready(crate::ConfigSourceLoad::Missing))
    }

    fn replace(
        &self,
        _expected_revision: Option<String>,
        _document: String,
    ) -> ConfigSourceReplaceFuture<'_> {
        Box::pin(std::future::ready(crate::ConfigSourceReplace::Unavailable(
            crate::ConfigSourceFailure::new(
                crate::ConfigSourceFailureKind::Unsafe,
                "MCP configuration source is unavailable",
            ),
        )))
    }
}

/// 不序列化、不明文调试的 MCP secret。
#[derive(Clone)]
pub struct McpSecret(String);

impl McpSecret {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// 只供 Host 在构造受控 Transport 时读取。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for McpSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// 已经完成纯业务校验、可交给 Host 建立候选连接的单 Server 配置。
#[derive(Clone)]
pub struct McpServerConfig {
    pub(crate) server_key: McpServerKey,
    pub(crate) display_name: String,
    pub(crate) description: String,
    pub(crate) enabled: bool,
    pub(crate) transport: McpServerTransportConfig,
    pub(crate) startup_timeout: Option<Duration>,
    pub(crate) tool_timeout: Option<Duration>,
    pub(crate) fingerprint: [u8; 32],
}

impl McpServerConfig {
    pub fn server_key(&self) -> &McpServerKey {
        &self.server_key
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn transport(&self) -> &McpServerTransportConfig {
        &self.transport
    }

    pub fn startup_timeout(&self) -> Option<Duration> {
        self.startup_timeout
    }

    pub fn tool_timeout(&self) -> Option<Duration> {
        self.tool_timeout
    }

    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    pub fn transport_kind(&self) -> McpTransportKind {
        match self.transport {
            McpServerTransportConfig::Stdio { .. } => McpTransportKind::Stdio,
            McpServerTransportConfig::StreamableHttp { .. } => McpTransportKind::StreamableHttp,
        }
    }
}

/// Host 建立 Transport 所需的最小配置。类型不实现 Debug/Serialize，避免 secret 泄漏。
#[derive(Clone)]
pub enum McpServerTransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        environment: BTreeMap<String, McpSecret>,
    },
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, McpSecret>,
    },
}

/// Host 已转换为 Runtime 自有值的原始工具定义。
#[derive(Clone)]
pub struct McpToolDefinition {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Option<Value>,
}

pub struct McpToolPage {
    pub tools: Vec<McpToolDefinition>,
    pub next_cursor: Option<String>,
}

/// 单次连接使用的进程级硬上限；由 Runtime 的当前配置快照提供。
#[derive(Clone, Copy)]
pub struct McpConnectionOptions {
    pub max_concurrent_requests: usize,
    pub control_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpConnectionFailureKind {
    InvalidConfig,
    Connect,
    Protocol,
    Catalog,
    ToolCall,
    UnsupportedExtension,
    Cancelled,
    Close,
}

/// Host Adapter 的稳定脱敏错误；底层 URL、command、stderr 和协议正文不越过边界。
#[derive(Debug)]
pub struct McpConnectionError {
    kind: McpConnectionFailureKind,
    message: &'static str,
}

impl McpConnectionError {
    pub fn new(kind: McpConnectionFailureKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub fn kind(&self) -> McpConnectionFailureKind {
        self.kind
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for McpConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for McpConnectionError {}

pub type McpConnectionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, McpConnectionError>> + Send + 'a>>;

/// Host 已从 `rmcp` 类型转换出的单次调用结果；原始 `_meta` 不跨越边界。
pub struct McpRawCallResult {
    pub content: Vec<McpRawContent>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

/// MCP content block 的 Runtime 私有对应类型。
pub enum McpRawContent {
    Text {
        text: String,
    },
    Image {
        data_base64: String,
        media_type: String,
    },
    Audio,
    ResourceLink {
        uri: String,
        name: String,
        title: Option<String>,
        description: Option<String>,
        media_type: Option<String>,
        size: Option<u64>,
    },
    EmbeddedText {
        uri: String,
        media_type: Option<String>,
        text: String,
    },
    EmbeddedBlob,
    Unsupported,
}

/// 一条已完成 MCP 生命周期建立的候选连接。
pub trait McpConnection: Send + Sync {
    fn list_tools_page(
        &self,
        cursor: Option<String>,
        cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, McpToolPage>;

    fn call_tool_once(
        &self,
        tool_name: String,
        arguments: serde_json::Map<String, Value>,
        cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, McpRawCallResult>;

    fn close(&self, cancellation: CancellationToken) -> McpConnectionFuture<'_, ()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpImageMaterializationFailure {
    Unsupported,
    TooLarge,
    Cancelled,
    Failed,
}

pub type McpImageMaterializationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<ToolImageReference, McpImageMaterializationFailure>> + Send + 'a,
    >,
>;

/// Host 的 Session 私有 tool-image 写入边界。
pub trait McpImageMaterializer: Send + Sync {
    fn materialize<'a>(
        &'a self,
        session_tool_image_directory: &'a str,
        media_type: &'a str,
        bytes: &'a [u8],
        cancellation: &'a CancellationToken,
    ) -> McpImageMaterializationFuture<'a>;
}

pub(crate) struct UnavailableMcpImageMaterializer;

impl McpImageMaterializer for UnavailableMcpImageMaterializer {
    fn materialize<'a>(
        &'a self,
        _session_tool_image_directory: &'a str,
        _media_type: &'a str,
        _bytes: &'a [u8],
        _cancellation: &'a CancellationToken,
    ) -> McpImageMaterializationFuture<'a> {
        Box::pin(std::future::ready(Err(
            McpImageMaterializationFailure::Failed,
        )))
    }
}

/// Host 私有 Transport/进程实现的构造端口。
pub trait McpConnectionFactory: Send + Sync {
    fn connect(
        &self,
        server: McpServerConfig,
        options: McpConnectionOptions,
        cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, Arc<dyn McpConnection>>;
}

pub(crate) struct UnavailableMcpConnectionFactory;

impl McpConnectionFactory for UnavailableMcpConnectionFactory {
    fn connect(
        &self,
        _server: McpServerConfig,
        _options: McpConnectionOptions,
        _cancellation: CancellationToken,
    ) -> McpConnectionFuture<'_, Arc<dyn McpConnection>> {
        Box::pin(std::future::ready(Err(McpConnectionError::new(
            McpConnectionFailureKind::Connect,
            "MCP connection factory is unavailable",
        ))))
    }
}

/// Runtime 的单一 MCP 业务入口；配置与 Registry 保持各自明确的权威边界。
pub(crate) struct McpService {
    pub(crate) config_store: Arc<McpConfigStore>,
    pub(crate) connection_factory: Arc<dyn McpConnectionFactory>,
    pub(crate) image_materializer: Arc<dyn McpImageMaterializer>,
    pub(crate) registry: Arc<McpRegistry>,
    pub(crate) management_available: bool,
    pub(crate) test_gate: Mutex<()>,
    pub(crate) tests: std::sync::Mutex<
        std::collections::BTreeMap<
            assistant_protocol::IdempotencyKey,
            (std::time::Instant, CancellationToken),
        >,
    >,
}

impl McpService {
    pub(crate) fn unavailable() -> Self {
        let connection_factory: Arc<dyn McpConnectionFactory> =
            Arc::new(UnavailableMcpConnectionFactory);
        Self {
            config_store: Arc::new(McpConfigStore::new(Arc::new(MissingMcpConfigSource))),
            registry: Arc::new(McpRegistry::new(connection_factory.clone())),
            connection_factory,
            image_materializer: Arc::new(UnavailableMcpImageMaterializer),
            management_available: false,
            test_gate: Mutex::new(()),
            tests: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    pub(crate) fn available(
        source: Arc<dyn McpConfigSource>,
        connection_factory: Arc<dyn McpConnectionFactory>,
        image_materializer: Arc<dyn McpImageMaterializer>,
    ) -> Self {
        Self {
            config_store: Arc::new(McpConfigStore::new(source)),
            registry: Arc::new(McpRegistry::new(connection_factory.clone())),
            connection_factory,
            image_materializer,
            management_available: true,
            test_gate: Mutex::new(()),
            tests: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    pub(crate) fn ensure_available(&self) -> RuntimeResult<()> {
        if self.management_available {
            Ok(())
        } else {
            Err(RuntimeError::InvalidRequest {
                reason: "MCP management capability is not available",
            })
        }
    }

    pub(crate) fn management_available(&self) -> bool {
        self.management_available
    }
}

pub(crate) fn effective_connect_timeout(
    runtime: McpRuntimeConfig,
    server: &McpServerConfig,
) -> Duration {
    server
        .startup_timeout()
        .map_or(runtime.connect_timeout(), |timeout| {
            timeout.min(runtime.connect_timeout())
        })
}

pub(crate) fn effective_tool_timeout(
    runtime: McpRuntimeConfig,
    server: &McpServerConfig,
) -> Duration {
    // 全局调用时限是缺省值；已校验的 Server 配置允许按业务耗时延长或缩短。
    server.tool_timeout().unwrap_or(runtime.request_timeout())
}
