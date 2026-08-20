use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc};

use agent_types::{FileReference, ToolImageReference};
use tokio_util::sync::CancellationToken;

use crate::{ModelError, ModelService};

/// 单张已经完成公共预处理、可直接交给协议编码器的图片。
#[derive(Clone, Eq, PartialEq)]
pub struct PreparedModelImage {
    pub media_type: String,
    pub bytes: Arc<[u8]>,
}

impl fmt::Debug for PreparedModelImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedModelImage")
            .field("media_type", &self.media_type)
            .field("byte_length", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelImageResource {
    FileReference(FileReference),
    /// 已由上层工具授权、只在当前调用中使用的本地图片路径。
    ///
    /// 该资源不进入规范 Conversation；Host 只能在调用方已经完成文件授权后准备它。
    LocalFile {
        path: String,
    },
    ToolImage {
        directory: String,
        reference: ToolImageReference,
    },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PreparedModelImageKey {
    FileReference(String),
    ToolImage(String),
}

/// 一次请求中按资源来源与稳定引用索引的瞬时图片资源。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreparedModelImages(BTreeMap<PreparedModelImageKey, PreparedModelImage>);

impl PreparedModelImages {
    pub fn insert_file_reference(&mut self, readable_path: String, image: PreparedModelImage) {
        self.0
            .insert(PreparedModelImageKey::FileReference(readable_path), image);
    }

    pub fn get_file_reference(&self, readable_path: &str) -> Option<&PreparedModelImage> {
        self.0.get(&PreparedModelImageKey::FileReference(
            readable_path.to_owned(),
        ))
    }

    pub fn insert_tool_image(&mut self, relative_path: String, image: PreparedModelImage) {
        self.0
            .insert(PreparedModelImageKey::ToolImage(relative_path), image);
    }

    pub fn get_tool_image(&self, relative_path: &str) -> Option<&PreparedModelImage> {
        self.0
            .get(&PreparedModelImageKey::ToolImage(relative_path.to_owned()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelImagePreparation {
    Image(PreparedModelImage),
    NotImage,
}

pub type ModelImagePreparationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelImagePreparation, ModelError>> + Send + 'a>>;

/// Host 提供的受控图片资源解析与公共预处理边界。
pub trait ModelImagePreprocessor: Send + Sync {
    fn prepare<'a>(
        &'a self,
        resource: &'a ModelImageResource,
        cancellation: &'a CancellationToken,
    ) -> ModelImagePreparationFuture<'a>;
}

/// 模型工厂交回的协议服务和可选 Host 图片预处理能力。
pub struct ModelServiceBundle {
    pub model: Arc<dyn ModelService>,
    pub image_preprocessor: Option<Arc<dyn ModelImagePreprocessor>>,
}

impl ModelServiceBundle {
    pub fn text_only(model: Arc<dyn ModelService>) -> Self {
        Self {
            model,
            image_preprocessor: None,
        }
    }

    pub fn with_image_preprocessor(
        model: Arc<dyn ModelService>,
        image_preprocessor: Arc<dyn ModelImagePreprocessor>,
    ) -> Self {
        Self {
            model,
            image_preprocessor: Some(image_preprocessor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(byte: u8) -> PreparedModelImage {
        PreparedModelImage {
            media_type: "image/png".to_owned(),
            bytes: Arc::from([byte]),
        }
    }

    #[test]
    fn attachment_and_tool_image_keys_use_separate_namespaces() {
        let key = format!("{}.png", "a".repeat(64));
        let mut images = PreparedModelImages::default();
        images.insert_file_reference(key.clone(), image(1));
        images.insert_tool_image(key.clone(), image(2));

        assert_eq!(images.len(), 2);
        assert_eq!(images.get_file_reference(&key), Some(&image(1)));
        assert_eq!(images.get_tool_image(&key), Some(&image(2)));
    }
}
