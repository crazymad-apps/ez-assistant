use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::AbsolutePath;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadImageRequest {
    pub path: AbsolutePath,
}

pub type ImageMaterializationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<agent_types::ToolImageReference, ImageMaterializerError>>
            + Send
            + 'a,
    >,
>;

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ImageMaterializerError {
    #[error("image materialization was cancelled")]
    Cancelled,
    #[error("image source is not a regular file")]
    InvalidSource,
    #[error("image source exceeds the supported limit")]
    TooLarge,
    #[error("image source is not a supported static image")]
    Unsupported,
    #[error("image materialization failed")]
    Failed,
}

pub trait ImageMaterializer: Send + Sync {
    fn materialize<'a>(
        &'a self,
        request: ReadImageRequest,
        cancellation: &'a CancellationToken,
    ) -> ImageMaterializationFuture<'a>;
}

pub type SharedImageMaterializer = Arc<dyn ImageMaterializer>;

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
