//! Run 前模型服务与 System Prompt 的宿主注入边界。

mod discovery;
mod parameters;
pub use parameters::{
    ModelParameterError, ModelParameterSource, ResolvedModelParameters, resolve_model_parameters,
    validate_fixed_model_parameters,
};

pub use discovery::{
    DiscoveredModel, ModelDiscoveryError, ModelDiscoveryErrorKind, ModelDiscoveryFormat,
    ModelDiscoveryFuture, ModelDiscoveryRequest, ModelFeatureSupport, ModelParameters,
    ModelReasoningMode, ModelTokenLimit,
};

use std::{error::Error, future::Future, pin::Pin, sync::Arc, time::Duration};

use agent_core::ToolPolicy;
use agent_memory::{MemoryRecall, PinnedMemoryStore, RecallReferenceReader};
use agent_model::ModelServiceBundle;
use agent_tools::ToolSetSnapshot;
use agent_types::ProviderId;
use thiserror::Error;

use crate::{ModelProtocol, ResolvedModelCapabilities, SessionExecutionEnvironment};

/// Host 异步创建单个子任务临时空间的结果 Future。
pub type ChildTaskWorkspaceFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<Box<dyn ChildTaskWorkspaceLease>, ChildTaskWorkspaceError>>
            + Send
            + 'a,
    >,
>;

/// 子任务活动期间独占的临时空间 lease。
///
/// Runtime 只读取稳定绝对路径；真正的目录创建与 Drop 清理由 Host 实现。
pub trait ChildTaskWorkspaceLease: Send {
    fn path(&self) -> &str;
}

/// 子任务临时空间创建失败。
#[derive(Debug, Error)]
#[error("child task workspace could not be created")]
pub struct ChildTaskWorkspaceError {
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ChildTaskWorkspaceError {
    pub fn new() -> Self {
        Self { source: None }
    }

    pub fn with_source(source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            source: Some(Box::new(source)),
        }
    }
}

impl Default for ChildTaskWorkspaceError {
    fn default() -> Self {
        Self::new()
    }
}

/// Host 提供的 OS 临时目录边界；Runtime 不直接访问文件系统。
pub trait ChildTaskWorkspaceFactory: Send + Sync {
    fn create<'a>(
        &'a self,
        child_task_id: &'a assistant_protocol::ChildTaskId,
    ) -> ChildTaskWorkspaceFuture<'a>;
}

/// 交给具体 Provider 工厂的一次冻结模型构造输入。
///
/// 本类型不实现 `Debug`，因为它短暂借用 API Key。工厂不得保存借用或输出 credential。
pub struct ModelServiceFactoryRequest<'a> {
    /// 外部来源的冻结身份，仅供受信宿主处理当前请求拒绝；不含凭据。
    pub external_configuration: Option<&'a Arc<crate::ExternalModelConfiguration>>,
    pub provider: &'a ProviderId,
    pub protocol: ModelProtocol,
    pub capabilities: &'a ResolvedModelCapabilities,
    pub endpoint: &'a str,
    pub model: &'a str,
    pub api_key: &'a str,
    pub context_window_tokens: u64,
    /// 当前已编译思考模式的独立输入上限。
    pub max_input_tokens: Option<u64>,
    pub connect_timeout: Duration,
    /// 等待响应建立及相邻流 chunk 的最长时间；不是流式请求总预算。
    pub request_timeout: Duration,
}

/// 具体 ModelService 构造失败。
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ModelServiceFactoryError {
    message: &'static str,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ModelServiceFactoryError {
    /// 创建不携带底层错误的脱敏失败。
    pub fn new(message: &'static str) -> Self {
        Self {
            message,
            source: None,
        }
    }

    /// 保留底层错误链，但 Display 只使用固定安全消息。
    pub fn with_source(message: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message,
            source: Some(Box::new(source)),
        }
    }
}

/// 从 Runtime 已校验配置构造具体 Provider 模型服务。
pub trait ModelServiceFactory: Send + Sync {
    /// 在用户域装配时确定模型权威来源，此后不切换来源。
    fn configuration_source(&self) -> crate::ModelSource {
        crate::ModelSource::Local
    }

    /// 模型来源决定是否已就绪及是否允许本地编辑；普通 Runtime 业务不读取产品模式。
    fn ensure_available(&self) -> crate::RuntimeResult<()> {
        Ok(())
    }

    fn ensure_editable(&self) -> crate::RuntimeResult<()> {
        Ok(())
    }

    /// 获取本次在线目录。旧的纯推理测试工厂默认不具备此能力，不回退到静态目录。
    ///
    /// # Errors
    /// 返回结构化的配置、网络或解析错误；部分分页成功不构成一次成功结果。
    fn discover_models<'a>(
        &'a self,
        _request: ModelDiscoveryRequest<'a>,
    ) -> ModelDiscoveryFuture<'a> {
        Box::pin(async {
            Err(ModelDiscoveryError::new(
                ModelDiscoveryErrorKind::Unsupported,
            ))
        })
    }

    fn create_model(
        &self,
        request: ModelServiceFactoryRequest<'_>,
    ) -> Result<ModelServiceBundle, ModelServiceFactoryError>;
}

/// 一次 Run 冻结使用的工具定义、Host 基础设施策略及默认权限规则来源。
///
/// Bundle 不进入 Protocol 或 Conversation；不同 Run 必须分别由
/// [`RunToolFactory`] 编译，不得修改共享的可变路径解析器。
pub struct RunToolBundle {
    tools: ToolSetSnapshot,
    infrastructure_policies: Vec<Arc<dyn ToolPolicy>>,
    default_rules: Option<Arc<dyn crate::PermissionRuleSource>>,
}

/// Host 编译单次 Run 工具所需的 Runtime 绑定能力。
pub struct RunToolFactoryRequest<'a> {
    pub session_id: &'a assistant_protocol::SessionId,
    pub environment: &'a SessionExecutionEnvironment,
    /// Runtime 在队首准备阶段可靠冻结，后续重建工具不能再次读取全局默认。
    pub shell: Option<&'a crate::FrozenShellEnvironment>,
    pub pinned_memory: Arc<dyn PinnedMemoryStore>,
    pub conversation_recall: Arc<dyn MemoryRecall>,
    pub conversation_recall_reader: Arc<dyn RecallReferenceReader>,
    /// Runtime 已按主/辅助模型能力判定后的可选识图能力。
    pub image_inspector: Option<agent_tools::SharedImageInspector>,
    /// 当前精确主模型路由是否允许注册原生图片读取工具。
    pub read_image_enabled: bool,
}

impl RunToolBundle {
    pub fn new(tools: ToolSetSnapshot, infrastructure_policies: Vec<Arc<dyn ToolPolicy>>) -> Self {
        Self {
            tools,
            infrastructure_policies,
            default_rules: None,
        }
    }

    /// 注入宿主默认权限规则；不落入用户可编辑的权限文件。
    pub fn with_default_rules(mut self, source: Arc<dyn crate::PermissionRuleSource>) -> Self {
        self.default_rules = Some(source);
        self
    }

    /// 默认规则来源按用户装配，父子执行共享；授权时读取最新规则。
    pub fn default_rules(&self) -> Option<Arc<dyn crate::PermissionRuleSource>> {
        self.default_rules.clone()
    }

    /// 消费 Bundle，取回本 Run 的不可变工具集和 Host 基础设施策略。
    pub fn into_parts(self) -> (ToolSetSnapshot, Vec<Arc<dyn ToolPolicy>>) {
        (self.tools, self.infrastructure_policies)
    }
}

/// Host 构造单次 Run 工具时的失败分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunToolFactoryErrorKind {
    /// Session 冻结的默认工作目录当前不可用。
    WorkingDirectoryUnavailable,
    /// 路径、工具配置或注册无法形成不可变 Bundle。
    InvalidConfiguration,
}

/// 单次 Run 工具编译失败。
#[derive(Debug, Error)]
#[error("run tools could not be created")]
pub struct RunToolFactoryError {
    kind: RunToolFactoryErrorKind,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl RunToolFactoryError {
    pub fn new(kind: RunToolFactoryErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn with_source(
        kind: RunToolFactoryErrorKind,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    pub fn kind(&self) -> RunToolFactoryErrorKind {
        self.kind
    }
}

/// 根据 Session 创建时冻结的目录事实编译单次 Run 工具。
pub trait RunToolFactory: Send + Sync {
    /// 宿主固定解释器目录；不提供真实 Shell 的验证宿主默认返回空目录。
    /// 具体实现若访问文件或注册表，调用方必须在阻塞任务边界执行。
    fn shell_catalog(&self) -> Vec<assistant_protocol::ShellCatalogEntry> {
        Vec::new()
    }

    /// 从受限枚举编译启动事实；None 表示平台默认，不允许静默回退显式目标。
    /// 返回 None 仅供未装配真实 Shell 的宿主使用。
    ///
    /// # Errors
    /// 目标未安装、平台不支持或配置无效时返回构造错误。
    fn freeze_shell(
        &self,
        kind: Option<assistant_protocol::ShellKind>,
    ) -> Result<Option<crate::FrozenShellEnvironment>, RunToolFactoryError> {
        if kind.is_some() {
            return Err(RunToolFactoryError::new(
                RunToolFactoryErrorKind::InvalidConfiguration,
            ));
        }
        Ok(None)
    }

    fn compile(
        &self,
        request: RunToolFactoryRequest<'_>,
    ) -> Result<RunToolBundle, RunToolFactoryError>;
}
