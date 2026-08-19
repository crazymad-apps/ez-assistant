//! Host 唯一图片嗅探、解码、模型预处理与缩略图实现。

use std::{
    fs,
    io::{BufReader, Cursor},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use agent_model::{
    ModelError, ModelImagePreparation, ModelImagePreparationFuture, ModelImagePreprocessor,
    PreparedModelImage,
};
use agent_types::FileReference;
use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, Rgb,
    RgbImage,
    codecs::{gif::GifDecoder, jpeg::JpegEncoder, png::PngDecoder, webp::WebPDecoder},
    imageops::FilterType,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MODEL_MAX_EDGE: u32 = 2_048;
const THUMBNAIL_MAX_EDGE: u32 = 320;
const MAX_SOURCE_PIXELS: u64 = 40_000_000;
const MAX_SOURCE_EDGE: u32 = 16_384;
const MAX_ASPECT_RATIO: u32 = 100;
const MAX_MODEL_BYTES: usize = 5 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const BACKGROUND: [u8; 3] = [248, 248, 248];

#[derive(Debug, Error)]
pub(crate) enum ImageResourceError {
    #[error("resource is outside the attachment registry boundary")]
    Boundary,
    #[error("resource is unavailable")]
    Unavailable,
    #[error("image format is unsupported")]
    Unsupported,
    #[error("animated images are unsupported")]
    Animated,
    #[error("image dimensions exceed the safe decoding limit")]
    Dimensions,
    #[error("image data is invalid")]
    Invalid,
    #[error("image cannot satisfy the common output limit")]
    OutputLimit,
}

/// 按文件签名返回实际 MIME；未知内容仍记录通用二进制类型。
pub(crate) fn sniff_media_type(path: &Path) -> Result<String, ImageResourceError> {
    infer::get_from_path(path)
        .map_err(|_| ImageResourceError::Unavailable)
        .map(|kind| {
            kind.map_or_else(
                || "application/octet-stream".to_owned(),
                |kind| kind.mime_type().to_owned(),
            )
        })
}

#[derive(Clone)]
pub(crate) struct HostModelImagePreprocessor {
    runtime_home: PathBuf,
}

impl HostModelImagePreprocessor {
    pub(crate) fn new(runtime_home: &Path) -> Self {
        Self {
            runtime_home: runtime_home.to_path_buf(),
        }
    }

    fn resolve(&self, readable_path: &str) -> Result<PathBuf, ImageResourceError> {
        let sessions = self.runtime_home.join("data/sessions");
        let candidate = Path::new(readable_path);
        let relative = candidate
            .strip_prefix(&sessions)
            .map_err(|_| ImageResourceError::Boundary)?;
        let parts = relative.components().collect::<Vec<_>>();
        if parts.len() != 4
            || !matches!(parts[0], Component::Normal(_))
            || parts[1].as_os_str() != "attachments"
            || !matches!(parts[2], Component::Normal(_))
            || !matches!(parts[3], Component::Normal(_))
        {
            return Err(ImageResourceError::Boundary);
        }
        let metadata =
            fs::symlink_metadata(candidate).map_err(|_| ImageResourceError::Unavailable)?;
        if !metadata.file_type().is_symlink() && !metadata.file_type().is_file() {
            return Err(ImageResourceError::Boundary);
        }
        let resolved = fs::canonicalize(candidate).map_err(|_| ImageResourceError::Unavailable)?;
        let blobs = fs::canonicalize(self.runtime_home.join("data/blobs"))
            .map_err(|_| ImageResourceError::Unavailable)?;
        if !resolved.starts_with(blobs) || !resolved.is_file() {
            return Err(ImageResourceError::Boundary);
        }
        Ok(resolved)
    }
}

impl ModelImagePreprocessor for HostModelImagePreprocessor {
    fn prepare<'a>(
        &'a self,
        reference: &'a FileReference,
        cancellation: &'a CancellationToken,
    ) -> ModelImagePreparationFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let path = self.resolve(&reference.readable_path).map_err(|error| {
                ModelError::Resource(format!("image `{}`: {error}", reference.original_name))
            })?;
            let name = reference.original_name.clone();
            let task = tokio::task::spawn_blocking(move || prepare_model_image(&path));
            tokio::select! {
                result = task => match result {
                    Ok(Ok(Some(image))) => Ok(ModelImagePreparation::Image(image)),
                    Ok(Ok(None)) => Ok(ModelImagePreparation::NotImage),
                    Ok(Err(error)) => Err(ModelError::Resource(format!("image `{name}`: {error}"))),
                    Err(_) => Err(ModelError::Resource(format!("image `{name}`: preprocessing failed"))),
                },
                () = cancellation.cancelled() => Err(ModelError::Cancelled),
            }
        })
    }
}

pub(crate) fn ensure_thumbnail(readable_path: &Path) -> Result<PathBuf, ImageResourceError> {
    let original = fs::canonicalize(readable_path).map_err(|_| ImageResourceError::Unavailable)?;
    let file_name = original
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ImageResourceError::Unavailable)?;
    let thumbnail = original.with_file_name(format!("{file_name}.thumbnail.jpg"));
    if thumbnail.is_file() {
        return Ok(thumbnail);
    }
    let Some(mut image) = decode_normalized(&original)? else {
        return Err(ImageResourceError::Unsupported);
    };
    image = image.resize(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE, FilterType::Lanczos3);
    let bytes = encode_jpeg(&image, 82)?;
    let temporary = thumbnail.with_extension("jpg.part");
    fs::write(&temporary, bytes).map_err(|_| ImageResourceError::Unavailable)?;
    if fs::rename(&temporary, &thumbnail).is_err() && !thumbnail.is_file() {
        return Err(ImageResourceError::Unavailable);
    }
    Ok(thumbnail)
}

fn prepare_model_image(path: &Path) -> Result<Option<PreparedModelImage>, ImageResourceError> {
    let Some(mut image) = decode_normalized(path)? else {
        return Ok(None);
    };
    image = image.resize(MODEL_MAX_EDGE, MODEL_MAX_EDGE, FilterType::Lanczos3);
    for quality in [86, 78, 70, 62, 54, 46] {
        let bytes = encode_jpeg(&image, quality)?;
        if bytes.len() <= MAX_MODEL_BYTES {
            return Ok(Some(PreparedModelImage {
                media_type: "image/jpeg".to_owned(),
                bytes: Arc::from(bytes),
            }));
        }
    }
    Err(ImageResourceError::OutputLimit)
}

fn decode_normalized(path: &Path) -> Result<Option<DynamicImage>, ImageResourceError> {
    let media_type = sniff_media_type(path)?;
    let format = match media_type.as_str() {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::WebP,
        "image/gif" => ImageFormat::Gif,
        value if value.starts_with("image/") => return Ok(None),
        _ => return Ok(None),
    };
    if fs::metadata(path)
        .map_err(|_| ImageResourceError::Unavailable)?
        .len()
        > MAX_SOURCE_BYTES
    {
        return Err(ImageResourceError::Dimensions);
    }
    let bytes = fs::read(path).map_err(|_| ImageResourceError::Unavailable)?;
    reject_animation(format, &bytes)?;
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .map_err(|_| ImageResourceError::Invalid)?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SOURCE_EDGE
        || height > MAX_SOURCE_EDGE
        || pixels > MAX_SOURCE_PIXELS
        || width.max(height) / width.min(height) > MAX_ASPECT_RATIO
    {
        return Err(ImageResourceError::Dimensions);
    }
    let orientation = decoder
        .orientation()
        .map_err(|_| ImageResourceError::Invalid)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| ImageResourceError::Invalid)?;
    image.apply_orientation(orientation);
    Ok(Some(composite_alpha(image)))
}

fn reject_animation(format: ImageFormat, bytes: &[u8]) -> Result<(), ImageResourceError> {
    let animated = match format {
        ImageFormat::Gif => {
            let decoder = GifDecoder::new(BufReader::new(Cursor::new(bytes)))
                .map_err(|_| ImageResourceError::Invalid)?;
            validate_dimensions(decoder.dimensions())?;
            decoder.into_frames().take(2).count() > 1
        }
        ImageFormat::Png => PngDecoder::new(Cursor::new(bytes))
            .map_err(|_| ImageResourceError::Invalid)?
            .is_apng()
            .map_err(|_| ImageResourceError::Invalid)?,
        ImageFormat::WebP => WebPDecoder::new(BufReader::new(Cursor::new(bytes)))
            .map_err(|_| ImageResourceError::Invalid)?
            .has_animation(),
        _ => false,
    };
    if animated {
        Err(ImageResourceError::Animated)
    } else {
        Ok(())
    }
}

fn validate_dimensions((width, height): (u32, u32)) -> Result<(), ImageResourceError> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SOURCE_EDGE
        || height > MAX_SOURCE_EDGE
        || pixels > MAX_SOURCE_PIXELS
        || width.max(height) / width.min(height) > MAX_ASPECT_RATIO
    {
        return Err(ImageResourceError::Dimensions);
    }
    Ok(())
}

fn composite_alpha(image: DynamicImage) -> DynamicImage {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut rgb = RgbImage::new(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u16::from(pixel[3]);
        let inverse = 255 - alpha;
        let blend = |channel: usize| {
            ((u16::from(pixel[channel]) * alpha + u16::from(BACKGROUND[channel]) * inverse + 127)
                / 255) as u8
        };
        rgb.put_pixel(x, y, Rgb([blend(0), blend(1), blend(2)]));
    }
    DynamicImage::ImageRgb8(rgb)
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, ImageResourceError> {
    let rgb = image.to_rgb8();
    let (width, height) = image.dimensions();
    let mut output = Vec::new();
    JpegEncoder::new_with_quality(&mut output, quality)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|_| ImageResourceError::Invalid)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use image::{
        Delay, Frame,
        codecs::gif::{GifEncoder, Repeat},
    };

    use super::*;

    #[test]
    fn transparent_png_is_normalized_to_bounded_jpeg() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("transparent.png");
        let mut image = image::RgbaImage::new(32, 16);
        image.put_pixel(0, 0, image::Rgba([255, 0, 0, 128]));
        image.save(&path).expect("save png");

        let prepared = prepare_model_image(&path).expect("prepare").expect("image");
        assert_eq!(prepared.media_type, "image/jpeg");
        assert!(prepared.bytes.len() <= MAX_MODEL_BYTES);
        assert_eq!(&prepared.bytes[..2], &[0xff, 0xd8]);
    }

    #[test]
    fn ordinary_file_is_not_misclassified_as_an_image() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("notes.txt");
        fs::write(&path, b"hello").expect("write text");
        assert_eq!(prepare_model_image(&path).expect("prepare"), None);
    }

    #[test]
    fn static_gif_is_supported_but_animation_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let static_path = directory.path().join("static.gif");
        DynamicImage::new_rgb8(8, 8)
            .save(&static_path)
            .expect("save static gif");
        assert!(
            prepare_model_image(&static_path)
                .expect("static gif")
                .is_some()
        );

        let animated_path = directory.path().join("animated.gif");
        let file = fs::File::create(&animated_path).expect("create animated gif");
        let mut encoder = GifEncoder::new(file);
        encoder.set_repeat(Repeat::Infinite).expect("repeat");
        encoder
            .encode_frames([
                Frame::from_parts(
                    image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ),
                Frame::from_parts(
                    image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 255, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ),
            ])
            .expect("encode animation");
        drop(encoder);

        assert!(matches!(
            prepare_model_image(&animated_path),
            Err(ImageResourceError::Animated)
        ));
    }

    #[test]
    fn dimensions_and_thumbnail_name_follow_common_limits() {
        assert!(validate_dimensions((2_048, 2_048)).is_ok());
        assert!(matches!(
            validate_dimensions((MAX_SOURCE_EDGE + 1, 1)),
            Err(ImageResourceError::Dimensions)
        ));

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("blob-name");
        DynamicImage::new_rgb8(640, 480)
            .save_with_format(&path, ImageFormat::Png)
            .expect("save source");
        let thumbnail = ensure_thumbnail(&path).expect("thumbnail");
        assert_eq!(
            thumbnail.file_name().and_then(|value| value.to_str()),
            Some("blob-name.thumbnail.jpg")
        );
        assert!(thumbnail.is_file());
    }
}
