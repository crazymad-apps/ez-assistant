//! Chat 与后续协议实现可复用的传输基础设施。

use base64::{Engine as _, engine::general_purpose::STANDARD};

mod credential;
mod endpoint;
mod sse;
mod tool_schema;
mod transport;

#[cfg(test)]
mod tests;

pub use credential::BearerCredential;
pub(crate) use endpoint::{join_endpoint, route_fingerprint, validate_base_url};
pub use sse::{SseFrame, SseParser};
pub use tool_schema::ToolSchemaDialect;
pub(crate) use tool_schema::encode_tool_schema;
pub use transport::{
    BodyStream, ObservedTransport, ProviderWireEvent, ProviderWireObserver, RecordedWireRequest,
    ReqwestTransport, Transport, TransportError, TransportFuture, TransportRequest,
    TransportResponse, TransportTimeouts,
};

pub(crate) fn image_data_url(image: &agent_model::PreparedModelImage) -> String {
    format!(
        "data:{};base64,{}",
        image.media_type,
        STANDARD.encode(image.bytes.as_ref())
    )
}
