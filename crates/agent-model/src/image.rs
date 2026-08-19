use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc};

use agent_types::FileReference;
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

/// 一次请求中按稳定文件引用索引的瞬时图片资源。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreparedModelImages(BTreeMap<String, PreparedModelImage>);

impl PreparedModelImages {
    pub fn insert(&mut self, readable_path: String, image: PreparedModelImage) {
        self.0.insert(readable_path, image);
    }

    pub fn get(&self, readable_path: &str) -> Option<&PreparedModelImage> {
        self.0.get(readable_path)
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
        reference: &'a FileReference,
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
