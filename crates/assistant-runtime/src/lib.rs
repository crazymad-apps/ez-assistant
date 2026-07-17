//! Assistant 应用运行时。
//!
//! Runtime 持有业务状态，并负责协调多会话、Agent Run、定时任务和配置。
//! 第一阶段它将直接运行在 Tauri 主进程内。

pub mod config;
pub mod run;
pub mod scheduler;
pub mod session;
