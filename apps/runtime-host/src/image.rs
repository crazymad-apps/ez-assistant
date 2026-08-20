//! Host 唯一图片嗅探、解码、模型预处理与缩略图实现。

use std::{
    fs,
    io::{BufReader, Cursor, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use agent_model::{
    ModelError, ModelImagePreparation, ModelImagePreparationFuture, ModelImagePreprocessor,
    ModelImageResource, PreparedModelImage,
};
use agent_tools::{
    ImageMaterializationFuture, ImageMaterializer, ImageMaterializerError, ReadImageRequest,
};
use agent_types::ToolImageReference;
use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, Rgb,
    RgbImage,
    codecs::{gif::GifDecoder, jpeg::JpegEncoder, png::PngDecoder, webp::WebPDecoder},
    imageops::FilterType,
};
use sha2::{Digest, Sha256};
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

#[derive(Clone)]
pub(crate) struct SessionImageMaterializer {
    directory: PathBuf,
}

impl SessionImageMaterializer {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
}

impl ImageMaterializer for SessionImageMaterializer {
    fn materialize<'a>(
        &'a self,
        request: ReadImageRequest,
        cancellation: &'a CancellationToken,
    ) -> ImageMaterializationFuture<'a> {
        Box::pin(async move {
            let bytes =
                agent_tools_local::read_binary_file(&request.path, MAX_SOURCE_BYTES, cancellation)
                    .await
                    .map_err(|error| match error {
                        agent_tools_local::BinaryReadError::Cancelled => {
                            ImageMaterializerError::Cancelled
                        }
                        agent_tools_local::BinaryReadError::NotRegularFile => {
                            ImageMaterializerError::InvalidSource
                        }
                        agent_tools_local::BinaryReadError::TooLarge => {
                            ImageMaterializerError::TooLarge
                        }
                        _ => ImageMaterializerError::Failed,
                    })?;
            if cancellation.is_cancelled() {
                return Err(ImageMaterializerError::Cancelled);
            }
            let directory = self.directory.clone();
            let stored =
                tokio::task::spawn_blocking(move || store_tool_image_bytes(&directory, &bytes))
                    .await
                    .map_err(|_| ImageMaterializerError::Failed)?
                    .map_err(|error| match error {
                        ImageResourceError::Animated
                        | ImageResourceError::Unsupported
                        | ImageResourceError::Invalid => ImageMaterializerError::Unsupported,
                        ImageResourceError::Dimensions | ImageResourceError::OutputLimit => {
                            ImageMaterializerError::TooLarge
                        }
                        _ => ImageMaterializerError::Failed,
                    })?;
            Ok(stored)
        })
    }
}

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
    #[error("tool image storage conflicts with the expected content")]
    Conflict,
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

    fn resolve_tool_image(
        &self,
        directory: &str,
        reference: &ToolImageReference,
    ) -> Result<PathBuf, ImageResourceError> {
        let sessions = self.runtime_home.join("data/sessions");
        let directory = Path::new(directory);
        let relative = directory
            .strip_prefix(&sessions)
            .map_err(|_| ImageResourceError::Boundary)?;
        let components = relative.components().collect::<Vec<_>>();
        if components.len() != 2
            || !matches!(components[0], Component::Normal(_))
            || components[1].as_os_str() != "tool-images"
        {
            return Err(ImageResourceError::Boundary);
        }
        validate_tool_image_file(directory, reference)?;
        Ok(directory.join(reference.relative_path()))
    }

    fn resolve_local_file(&self, path: &str) -> Result<PathBuf, ImageResourceError> {
        let candidate = Path::new(path);
        if !candidate.is_absolute() {
            return Err(ImageResourceError::Boundary);
        }
        let metadata = fs::metadata(candidate).map_err(|_| ImageResourceError::Unavailable)?;
        if !metadata.is_file() {
            return Err(ImageResourceError::Boundary);
        }
        Ok(candidate.to_path_buf())
    }
}

impl ModelImagePreprocessor for HostModelImagePreprocessor {
    fn prepare<'a>(
        &'a self,
        resource: &'a ModelImageResource,
        cancellation: &'a CancellationToken,
    ) -> ModelImagePreparationFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }
            let (path, name, must_be_image) = match resource {
                ModelImageResource::FileReference(reference) => (
                    self.resolve(&reference.readable_path).map_err(|error| {
                        ModelError::Resource(format!(
                            "image `{}`: {error}",
                            reference.original_name
                        ))
                    })?,
                    reference.original_name.clone(),
                    false,
                ),
                ModelImageResource::LocalFile { path } => (
                    self.resolve_local_file(path)
                        .map_err(|error| ModelError::Resource(format!("local image: {error}")))?,
                    Path::new(path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("local image")
                        .to_owned(),
                    true,
                ),
                ModelImageResource::ToolImage {
                    directory,
                    reference,
                } => (
                    self.resolve_tool_image(directory, reference)
                        .map_err(|error| ModelError::Resource(format!("tool image: {error}")))?,
                    "tool image".to_owned(),
                    true,
                ),
            };
            let task = tokio::task::spawn_blocking(move || prepare_model_image(&path));
            tokio::select! {
                result = task => match result {
                    Ok(Ok(Some(image))) => Ok(ModelImagePreparation::Image(image)),
                    Ok(Ok(None)) if !must_be_image => Ok(ModelImagePreparation::NotImage),
                    Ok(Ok(None)) => Err(ModelError::Resource("tool image: unsupported image".to_owned())),
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

/// 校验 Session Tool Image 后在内存中生成有界 JPEG；不得在 `tool-images/` 落缩略图副本。
pub(crate) fn tool_image_thumbnail(
    directory: &Path,
    reference: &ToolImageReference,
) -> Result<Vec<u8>, ImageResourceError> {
    validate_tool_image_file(directory, reference)?;
    let path = directory.join(reference.relative_path());
    let Some(mut image) = decode_normalized(&path)? else {
        return Err(ImageResourceError::Unsupported);
    };
    image = image.resize(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE, FilterType::Lanczos3);
    encode_jpeg(&image, 82)
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
    if fs::metadata(path)
        .map_err(|_| ImageResourceError::Unavailable)?
        .len()
        > MAX_SOURCE_BYTES
    {
        return Err(ImageResourceError::Dimensions);
    }
    let bytes = fs::read(path).map_err(|_| ImageResourceError::Unavailable)?;
    decode_normalized_bytes(&bytes)
}

fn decode_normalized_bytes(bytes: &[u8]) -> Result<Option<DynamicImage>, ImageResourceError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SOURCE_BYTES {
        return Err(ImageResourceError::Dimensions);
    }
    let media_type = sniff_media_type_bytes(bytes);
    let format = match media_type.as_str() {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::WebP,
        "image/gif" => ImageFormat::Gif,
        value if value.starts_with("image/") => return Ok(None),
        _ => return Ok(None),
    };
    reject_animation(format, bytes)?;
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

fn sniff_media_type_bytes(bytes: &[u8]) -> String {
    infer::get(bytes).map_or_else(
        || "application/octet-stream".to_owned(),
        |kind| kind.mime_type().to_owned(),
    )
}

/// 校验并保存一份已读取的静态图片原始字节，返回 Session 内稳定引用。
pub(crate) fn store_tool_image_bytes(
    directory: &Path,
    bytes: &[u8],
) -> Result<ToolImageReference, ImageResourceError> {
    let media_type = sniff_media_type_bytes(bytes);
    if decode_normalized_bytes(bytes)?.is_none() {
        return Err(ImageResourceError::Unsupported);
    }
    let digest = Sha256::digest(bytes);
    let hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let extension = match media_type.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return Err(ImageResourceError::Unsupported),
    };
    let reference = ToolImageReference::new(format!("{hash}.{extension}"), media_type)
        .map_err(|_| ImageResourceError::Invalid)?;
    commit_tool_image(directory, bytes, &reference)?;
    Ok(reference)
}

/// 验证一个引用在指定 Session 根下仍是匹配哈希、MIME 与解码限制的普通文件。
pub(crate) fn validate_tool_image_file(
    directory: &Path,
    reference: &ToolImageReference,
) -> Result<(), ImageResourceError> {
    let path = directory.join(reference.relative_path());
    let metadata = fs::symlink_metadata(&path).map_err(|_| ImageResourceError::Unavailable)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return Err(ImageResourceError::Unavailable);
    }
    let bytes = fs::read(&path).map_err(|_| ImageResourceError::Unavailable)?;
    let actual = store_reference_for_bytes(&bytes)?;
    if &actual != reference {
        return Err(ImageResourceError::Conflict);
    }
    Ok(())
}

/// 将源 Session 的稳定图片复制到目标 Session，不建立跨 Session 链接或引用计数。
pub(crate) fn copy_tool_image(
    source_directory: &Path,
    target_directory: &Path,
    reference: &ToolImageReference,
) -> Result<(), ImageResourceError> {
    validate_tool_image_file(source_directory, reference)?;
    let bytes = fs::read(source_directory.join(reference.relative_path()))
        .map_err(|_| ImageResourceError::Unavailable)?;
    let stored = store_tool_image_bytes(target_directory, &bytes)?;
    if &stored != reference {
        return Err(ImageResourceError::Conflict);
    }
    Ok(())
}

fn store_reference_for_bytes(bytes: &[u8]) -> Result<ToolImageReference, ImageResourceError> {
    let media_type = sniff_media_type_bytes(bytes);
    if decode_normalized_bytes(bytes)?.is_none() {
        return Err(ImageResourceError::Unsupported);
    }
    let digest = Sha256::digest(bytes);
    let hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let extension = match media_type.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return Err(ImageResourceError::Unsupported),
    };
    ToolImageReference::new(format!("{hash}.{extension}"), media_type)
        .map_err(|_| ImageResourceError::Invalid)
}

fn commit_tool_image(
    directory: &Path,
    bytes: &[u8],
    reference: &ToolImageReference,
) -> Result<(), ImageResourceError> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| ImageResourceError::Unavailable)?;
    if !metadata.file_type().is_dir() {
        return Err(ImageResourceError::Boundary);
    }
    let target = directory.join(reference.relative_path());
    if target.exists() {
        return validate_tool_image_file(directory, reference);
    }

    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| ImageResourceError::Unavailable)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temporary = directory.join(format!(".{suffix}.part"));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| ImageResourceError::Unavailable)?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|_| ImageResourceError::Unavailable)?;
        file.sync_all()
            .map_err(|_| ImageResourceError::Unavailable)?;
        match fs::hard_link(&temporary, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_tool_image_file(directory, reference)?;
            }
            Err(_) => return Err(ImageResourceError::Unavailable),
        }
        fs::set_permissions(&target, fs::Permissions::from_mode(0o400))
            .map_err(|_| ImageResourceError::Unavailable)?;
        fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ImageResourceError::Unavailable)?;
        validate_tool_image_file(directory, reference)
    })();
    drop(file);
    let _ = fs::remove_file(&temporary);
    result
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
    use std::{os::unix::fs::MetadataExt, sync::Arc};

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
    fn tool_image_thumbnail_is_in_memory_and_revalidates_the_reference() {
        let directory = tempfile::tempdir().expect("tempdir");
        let image = image::RgbImage::from_pixel(640, 320, image::Rgb([15, 25, 35]));
        let mut source = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut Cursor::new(&mut source), ImageFormat::Png)
            .expect("encode source");
        let reference = store_tool_image_bytes(directory.path(), &source).expect("store image");

        let thumbnail = tool_image_thumbnail(directory.path(), &reference).expect("thumbnail");
        assert_eq!(&thumbnail[..2], &[0xff, 0xd8]);
        let decoded = image::load_from_memory(&thumbnail).expect("decode thumbnail");
        assert_eq!(decoded.dimensions(), (320, 160));
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("tool image entries")
                .count(),
            1
        );

        let stored_path = directory.path().join(reference.relative_path());
        fs::set_permissions(&stored_path, fs::Permissions::from_mode(0o600))
            .expect("make image writable for corruption fixture");
        fs::write(stored_path, b"corrupt").expect("corrupt image");
        assert!(tool_image_thumbnail(directory.path(), &reference).is_err());
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

    #[test]
    fn tool_image_store_deduplicates_and_copies_between_sessions() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).expect("source directory");
        fs::create_dir(&target).expect("target directory");
        let image_path = root.path().join("fixture.png");
        DynamicImage::new_rgb8(16, 8)
            .save(&image_path)
            .expect("save image");
        let bytes = fs::read(&image_path).expect("read fixture");

        let first = store_tool_image_bytes(&source, &bytes).expect("first store");
        let second = store_tool_image_bytes(&source, &bytes).expect("deduplicated store");
        assert_eq!(first, second);
        assert_eq!(
            fs::read_dir(&source)
                .expect("source entries")
                .collect::<Result<Vec<_>, _>>()
                .expect("entries")
                .len(),
            1
        );
        assert!(
            fs::metadata(source.join(first.relative_path()))
                .expect("stored metadata")
                .permissions()
                .readonly()
        );

        copy_tool_image(&source, &target, &first).expect("copy image");
        validate_tool_image_file(&target, &first).expect("copied image");
        assert_ne!(
            fs::metadata(source.join(first.relative_path()))
                .expect("source metadata")
                .ino(),
            fs::metadata(target.join(first.relative_path()))
                .expect("target metadata")
                .ino()
        );
    }

    #[test]
    fn concurrent_tool_image_stores_publish_one_stable_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = root.path().join("tool-images");
        fs::create_dir(&directory).expect("tool image directory");
        let image_path = root.path().join("fixture.png");
        DynamicImage::new_rgb8(16, 8)
            .save(&image_path)
            .expect("save image");
        let bytes = Arc::new(fs::read(&image_path).expect("read fixture"));
        let directory = Arc::new(directory);

        let handles = (0..8)
            .map(|_| {
                let bytes = Arc::clone(&bytes);
                let directory = Arc::clone(&directory);
                std::thread::spawn(move || {
                    store_tool_image_bytes(&directory, &bytes).expect("store image")
                })
            })
            .collect::<Vec<_>>();
        let references = handles
            .into_iter()
            .map(|handle| handle.join().expect("store thread"))
            .collect::<Vec<_>>();

        assert!(references.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(
            fs::read_dir(directory.as_ref())
                .expect("tool image entries")
                .collect::<Result<Vec<_>, _>>()
                .expect("entries")
                .len(),
            1
        );
    }

    #[test]
    fn tool_image_store_rejects_animation_and_conflicting_stable_target() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = root.path().join("tool-images");
        fs::create_dir(&directory).expect("tool image directory");
        let animated_path = root.path().join("animated.gif");
        let file = fs::File::create(&animated_path).expect("create animated gif");
        let mut encoder = GifEncoder::new(file);
        encoder
            .encode_frames([
                Frame::new(image::RgbaImage::from_pixel(
                    4,
                    4,
                    image::Rgba([1, 2, 3, 255]),
                )),
                Frame::new(image::RgbaImage::from_pixel(
                    4,
                    4,
                    image::Rgba([4, 5, 6, 255]),
                )),
            ])
            .expect("animated gif");
        drop(encoder);
        assert!(matches!(
            store_tool_image_bytes(&directory, &fs::read(animated_path).expect("gif bytes")),
            Err(ImageResourceError::Animated)
        ));

        let png = root.path().join("fixture.png");
        DynamicImage::new_rgb8(8, 8).save(&png).expect("png");
        let bytes = fs::read(png).expect("png bytes");
        let reference = store_reference_for_bytes(&bytes).expect("reference");
        fs::write(directory.join(reference.relative_path()), b"corrupt").expect("corrupt target");
        assert!(matches!(
            store_tool_image_bytes(&directory, &bytes),
            Err(ImageResourceError::Unsupported | ImageResourceError::Conflict)
        ));
    }

    #[tokio::test]
    async fn model_preprocessor_resolves_tool_images_only_from_a_session_tool_root() {
        let root = tempfile::tempdir().expect("runtime home");
        let directory = root.path().join("data/sessions/session-image/tool-images");
        fs::create_dir_all(&directory).expect("tool image directory");
        let fixture = root.path().join("fixture-tool-image.png");
        DynamicImage::new_rgb8(8, 8)
            .save(&fixture)
            .expect("fixture image");
        let reference =
            store_tool_image_bytes(&directory, &fs::read(&fixture).expect("fixture bytes"))
                .expect("stored image");
        let preprocessor = HostModelImagePreprocessor::new(root.path());
        let resource = ModelImageResource::ToolImage {
            directory: directory.to_string_lossy().into_owned(),
            reference: reference.clone(),
        };

        assert!(matches!(
            preprocessor
                .prepare(&resource, &CancellationToken::new())
                .await,
            Ok(ModelImagePreparation::Image(_))
        ));

        let outside = ModelImageResource::ToolImage {
            directory: root.path().join("outside").to_string_lossy().into_owned(),
            reference,
        };
        assert!(matches!(
            preprocessor
                .prepare(&outside, &CancellationToken::new())
                .await,
            Err(ModelError::Resource(_))
        ));
    }
}
