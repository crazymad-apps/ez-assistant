//! MCP 图片结果到现有 Session tool-image 存储的 Host Adapter。

use std::path::{Path, PathBuf};

use assistant_runtime::{
    McpImageMaterializationFailure, McpImageMaterializationFuture, McpImageMaterializer,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub(crate) struct HostMcpImageMaterializer;

impl McpImageMaterializer for HostMcpImageMaterializer {
    fn materialize<'a>(
        &'a self,
        session_tool_image_directory: &'a str,
        media_type: &'a str,
        bytes: &'a [u8],
        cancellation: &'a CancellationToken,
    ) -> McpImageMaterializationFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(McpImageMaterializationFailure::Cancelled);
            }
            let directory = PathBuf::from(session_tool_image_directory);
            if !directory.is_absolute() || !Path::new(&directory).is_dir() {
                return Err(McpImageMaterializationFailure::Failed);
            }
            let media_type = media_type.to_owned();
            let bytes = bytes.to_vec();
            let reference = tokio::task::spawn_blocking(move || {
                crate::image::store_tool_image_bytes(&directory, &bytes)
            })
            .await
            .map_err(|_| McpImageMaterializationFailure::Failed)?
            .map_err(|error| match error {
                crate::image::ImageResourceError::Animated
                | crate::image::ImageResourceError::Unsupported
                | crate::image::ImageResourceError::Invalid => {
                    McpImageMaterializationFailure::Unsupported
                }
                crate::image::ImageResourceError::Dimensions
                | crate::image::ImageResourceError::OutputLimit => {
                    McpImageMaterializationFailure::TooLarge
                }
                _ => McpImageMaterializationFailure::Failed,
            })?;
            if cancellation.is_cancelled() {
                return Err(McpImageMaterializationFailure::Cancelled);
            }
            if reference.media_type() != media_type {
                return Err(McpImageMaterializationFailure::Unsupported);
            }
            Ok(reference)
        })
    }
}
