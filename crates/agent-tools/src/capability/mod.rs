//! 文件系统与 Shell 能力契约。
//!
//! 契约只描述能力形状与错误语义，不含任何安全策略；真实副作用由
//! Runtime/Adapter 实现并经桥接工具注册进 Registry。

pub mod fs;
pub mod shell;
