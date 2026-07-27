//! 内置工具桥接：把能力契约实现包装为类型化 Tool。
//!
//! 仅提供桥接构造器；向 Registry 注册哪些能力由 Runtime 装配决定。

pub mod fs;
pub mod shell;
