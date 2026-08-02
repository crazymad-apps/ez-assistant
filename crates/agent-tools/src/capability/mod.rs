//! 文件系统与 Shell 能力契约。
//!
//! 契约只描述能力形状与错误语义，不含任何安全策略；本地、远程或测试
//! 能力实现由 Runtime 或其他上层宿主注入标准工具壳并注册进 Registry。

pub mod fs;
pub mod shell;
