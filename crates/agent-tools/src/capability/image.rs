use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InspectImagesRequest {
    pub image_paths: Vec<String>,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageInspection {
    pub text: String,
    pub model_key: String,
    pub elapsed_ms: u64,
    pub usage: Option<agent_types::TokenUsage>,
}

pub type ImageInspectionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ImageInspection, ImageInspectorError>> + Send + 'a>>;

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ImageInspectorError {
    #[error("image inspection was cancelled")]
    Cancelled,
    #[error("image inspection timed out")]
    Timeout,
    #[error("image inspection failed")]
    Failed,
}

pub trait ImageInspector: Send + Sync {
    fn inspect<'a>(
        &'a self,
        request: InspectImagesRequest,
        cancellation: &'a CancellationToken,
    ) -> ImageInspectionFuture<'a>;
}

pub type SharedImageInspector = Arc<dyn ImageInspector>;
