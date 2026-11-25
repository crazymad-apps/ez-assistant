//! 智能终端 Gateway 的安装身份、线协议与连接生命周期。

mod connection;
mod console;
mod crypto;
mod dispatcher;
mod gateway;
mod identity;
mod mcp;
mod native_console;
mod protocol;

pub(crate) use console::{ConsoleProxyPathError, DeviceConsoleTarget, resolve_proxy_url};
pub(crate) use dispatcher::DeviceChannelOutputDispatcher;
pub(crate) use gateway::{DeviceGatewayHandle, DeviceGatewayService};
#[cfg(test)]
pub(crate) use protocol::DeviceMcpScheme;
