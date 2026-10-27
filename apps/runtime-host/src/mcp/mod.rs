//! Runtime Host 私有 MCP Adapter。

mod cleanup;
mod config_source;
mod connection;
mod http_client;
mod image;

pub(crate) use config_source::LocalMcpConfigSource;
pub(crate) use connection::{BoundedLineReader, HostMcpConnectionFactory, MAX_MESSAGE_BYTES};
pub(crate) use image::HostMcpImageMaterializer;
