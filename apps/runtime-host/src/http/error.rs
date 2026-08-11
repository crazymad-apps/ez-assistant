//! HTTP transport 错误与 Runtime 业务错误状态码的集中映射。

use assistant_protocol::RuntimeErrorCode;
use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

#[derive(Debug)]
pub(super) struct HttpError {
    status: StatusCode,
    info: TransportErrorInfo,
}

#[derive(Debug, Serialize)]
struct TransportErrorInfo {
    code: TransportErrorCode,
    message: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum TransportErrorCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
}

#[derive(Serialize)]
struct ErrorBody {
    error: TransportErrorInfo,
}

impl HttpError {
    pub(super) fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            TransportErrorCode::InvalidRequest,
            message,
        )
    }

    pub(super) fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            TransportErrorCode::Unauthorized,
            "runtime access token is missing or invalid",
        )
    }

    pub(super) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            TransportErrorCode::Forbidden,
            message,
        )
    }

    fn new(status: StatusCode, code: TransportErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            info: TransportErrorInfo {
                code,
                message: message.into(),
            },
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(ErrorBody { error: self.info })).into_response()
    }
}

pub(super) fn runtime_status(code: RuntimeErrorCode) -> StatusCode {
    match code {
        RuntimeErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
        RuntimeErrorCode::SessionNotFound
        | RuntimeErrorCode::RunNotFound
        | RuntimeErrorCode::InputNotFound
        | RuntimeErrorCode::ModelNotFound
        | RuntimeErrorCode::WorkspaceNotFound
        | RuntimeErrorCode::AttachmentNotFound => StatusCode::NOT_FOUND,
        RuntimeErrorCode::SessionBusy
        | RuntimeErrorCode::SessionArchived
        | RuntimeErrorCode::SessionNotIdle
        | RuntimeErrorCode::RunNotRetryable
        | RuntimeErrorCode::WorkspaceRemoved
        | RuntimeErrorCode::WorkspaceUnavailable
        | RuntimeErrorCode::AttachmentUnavailable => StatusCode::CONFLICT,
        RuntimeErrorCode::AttachmentTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        RuntimeErrorCode::AttachmentUploadInvalid => StatusCode::BAD_REQUEST,
        RuntimeErrorCode::StorageUnavailable
        | RuntimeErrorCode::RuntimeShuttingDown
        | RuntimeErrorCode::ConfigurationUnavailable
        | RuntimeErrorCode::ModelUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        RuntimeErrorCode::AgentBuildFailed
        | RuntimeErrorCode::ModelBuildFailed
        | RuntimeErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        RuntimeErrorCode::ModelExecutionFailed => StatusCode::BAD_GATEWAY,
    }
}
