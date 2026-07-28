//! 调试查看器：网页端实时查看模型、Agent 与 Runtime 数据流的独立开发工具。
//!
//! - `wire`：推送端与 server 之间的 JSON 线格式。
//! - `client`：出向推送客户端（fire-and-forget），供 CLI demo 与 Runtime Harness 使用。
//! - `server`：viewer HTTP 服务（POST 接收、SSE 广播、内嵌静态页）。
//!
//! 本 crate 是独立开发工具，不属于产品进程；产品进程只向它建立出向连接。
//! credential 永不进入调试 payload。

mod client;
mod server;
mod wire;

pub use client::DebugClient;
pub use server::{port_from_env, router, run};
pub use wire::{BroadcastMessage, DEFAULT_PORT, DebugChannel, DebugEnvelope, DebugPayload};
