//! 有界反馈传输。只有明确业务拒绝可断言未提交，其他 POST 异常均保留未知。
use assistant_protocol::{FeedbackFailure as Failure, SecretValue};
use futures_util::StreamExt as _;
use reqwest::{RequestBuilder, StatusCode};
use serde::{Serialize, de::DeserializeOwned};

use super::CenterClient;

impl CenterClient {
    pub(crate) async fn feedback_get<T: DeserializeOwned>(
        &self,
        token: &SecretValue,
        path: &str,
    ) -> Result<T, Failure> {
        decode(
            self.client
                .get(self.endpoint(path))
                .bearer_auth(token.expose()),
            false,
        )
        .await
    }
    pub(crate) async fn feedback_post<T: DeserializeOwned>(
        &self,
        token: &SecretValue,
        body: &impl Serialize,
    ) -> Result<T, Failure> {
        decode(
            self.client
                .post(self.endpoint("feedback"))
                .bearer_auth(token.expose())
                .json(body),
            true,
        )
        .await
    }
}
async fn decode<T: DeserializeOwned>(request: RequestBuilder, writing: bool) -> Result<T, Failure> {
    let uncertain = if writing {
        Failure::SubmissionUnknown
    } else {
        Failure::Unavailable
    };
    let response = request.send().await.map_err(|_| uncertain)?;
    let status = response.status();
    if response.content_length().is_some_and(|size| size > 65536) {
        return Err(uncertain);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| uncertain)?;
        if body.len() + chunk.len() > 65536 {
            return Err(uncertain);
        }
        body.extend_from_slice(&chunk);
    }
    if status == StatusCode::OK {
        return serde_json::from_slice(&body).map_err(|_| uncertain);
    }
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let code = value.pointer("/error/code").and_then(|code| code.as_str());
    Err(match (status, code) {
        (StatusCode::UNAUTHORIZED, Some("TOKEN_INVALID")) => Failure::IdentityChanged,
        (StatusCode::BAD_REQUEST, Some("INVALID_REQUEST")) => Failure::InvalidInput,
        (StatusCode::PAYLOAD_TOO_LARGE, Some("FEEDBACK_TOO_LARGE")) => Failure::TooLarge,
        (StatusCode::CONFLICT, Some("FEEDBACK_SUBMISSION_CONFLICT")) => Failure::Conflict,
        (StatusCode::NOT_FOUND, Some("FEEDBACK_NOT_FOUND")) => Failure::NotFound,
        _ => uncertain,
    })
}
