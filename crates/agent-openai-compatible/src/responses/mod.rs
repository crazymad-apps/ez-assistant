//! OpenAI Responses 协议方言、私有 Schema、Codec、流状态机与服务。

mod adapter;
mod decode;
mod encode;
mod schema;
mod service;
mod stream;

#[cfg(test)]
mod tests;

pub use adapter::{FunctionOutputShape, ResponsesProtocolAdapter};
pub use decode::decode_response;
pub use service::{OpenAiResponsesService, OpenAiResponsesServiceError};
