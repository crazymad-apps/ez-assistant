//! 智能终端 Gateway 的安装身份、线协议与连接生命周期。

mod connection;
mod crypto;
mod dispatcher;
mod gateway;
mod identity;
mod protocol;

pub(crate) use dispatcher::DeviceChannelOutputDispatcher;
pub(crate) use gateway::{DeviceGatewayHandle, DeviceGatewayService};
