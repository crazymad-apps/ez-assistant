//! Agent 执行引擎。
//!
//! 本 crate 只负责 Agent 推理循环及其直接依赖的模型、工具抽象；
//! 会话调度、定时任务和配置加载属于 `assistant-runtime`。

pub mod agent;
pub mod model;
pub mod tool;
