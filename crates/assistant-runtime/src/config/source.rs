//! Runtime 配置文档的可替换读取边界。
//!
//! 本模块只描述“读取到了什么”，不接触真实文件系统。生产 Host 负责路径、权限、文件类型和
//! 大小检查；Runtime 负责把结果编译并交换到唯一配置 Registry。

use std::{future::Future, pin::Pin};

/// 配置源无法交付文档时的安全分类。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigSourceFailureKind {
    /// 文件类型、权限、符号链接或大小不符合敏感配置要求。
    Unsafe,
    /// 安全检查通过前后发生 I/O 或文本解码失败。
    Read,
}

/// 不包含路径、原始文档或底层 I/O 文本的配置源失败。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigSourceFailure {
    kind: ConfigSourceFailureKind,
    message: &'static str,
}

impl ConfigSourceFailure {
    /// 创建一条已经脱敏的配置源失败。
    pub fn new(kind: ConfigSourceFailureKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    /// 稳定失败类别。
    pub fn kind(self) -> ConfigSourceFailureKind {
        self.kind
    }

    /// 可安全展示的固定消息。
    pub fn message(self) -> &'static str {
        self.message
    }
}

/// 配置源一次读取的完整结果。
///
/// 本类型刻意不实现 `Debug`，避免未来调试输出意外包含 `Document` 中的 API Key。
pub enum ConfigSourceLoad {
    /// 配置文件不存在。
    Missing,
    /// 已完成文件安全检查的 UTF-8 TOML 文档。
    Document(String),
    /// 文件存在但无法安全读取。
    Unavailable(ConfigSourceFailure),
}

/// 一次异步配置读取。
pub type ConfigSourceFuture<'a> = Pin<Box<dyn Future<Output = ConfigSourceLoad> + Send + 'a>>;

/// Runtime 唯一配置文档来源。
pub trait RuntimeConfigSource: Send + Sync {
    /// 可安全展示给本地客户端的配置路径；非文件测试源默认没有路径。
    fn display_path(&self) -> Option<String> {
        None
    }

    /// 读取当前配置；实现不得把原始文档或底层敏感错误写入日志。
    fn load(&self) -> ConfigSourceFuture<'_>;
}
