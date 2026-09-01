//! 有界命令队列与专用阻塞存储线程。
//!
//! `client` 把异步 RuntimeStore 调用转换为有界命令；`thread` 独占
//! StorageEngine 并同步分发；`command` 只定义两侧共享的类型化消息。

mod client;
mod command;
mod thread;

pub(crate) use client::LocalRuntimeStore;
