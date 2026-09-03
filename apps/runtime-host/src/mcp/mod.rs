//! Runtime Host 私有 MCP Adapter。

mod cleanup;
mod config_source;
mod connection;
mod http_client;
mod image;

pub(crate) use config_source::LocalMcpConfigSource;
pub(crate) use connection::HostMcpConnectionFactory;
pub(crate) use image::HostMcpImageMaterializer;
