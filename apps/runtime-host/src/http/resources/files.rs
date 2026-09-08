//! Host 文件读取基础。Session 先校验根，任意目录入口不附加 Session 边界；两者都从已打开句柄读取。

use assistant_protocol::{
    HostFileEntry, ListHostFilesResult, PreviewSessionResourceFileResult, RuntimeErrorCode,
    RuntimeErrorInfo, SessionResourceEntryKind, SessionResourceEntryState,
    SessionResourcePreviewKind,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rustix::fs::{Dir, Mode, OFlags, open, openat};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

pub(super) const MAX_DIRECTORY_ENTRIES: usize = 2_000;
const TEXT_LIMIT: u64 = 4 * 1024 * 1024;
const MEDIA_LIMIT: u64 = 16 * 1024 * 1024;
const GENERATED: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".build",
    "DerivedData",
    "coverage",
];

pub(super) fn error(error: impl Into<std::io::Error>) -> RuntimeErrorInfo {
    let error = error.into();
    let message = match error.kind() {
        std::io::ErrorKind::PermissionDenied => "没有权限读取此路径。",
        std::io::ErrorKind::NotFound => "路径不存在或符号链接已失效。",
        _ => "无法读取此路径，请检查文件类型、权限或路径是否已变化。",
    };
    RuntimeErrorInfo::new(RuntimeErrorCode::ResourceNotPreviewable, message)
}

pub(super) fn path_text(path: &Path) -> Result<String, RuntimeErrorInfo> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| super::invalid_request("路径包含无法显示的字符。"))
}

pub(super) fn host_path(value: Option<&str>) -> Result<PathBuf, RuntimeErrorInfo> {
    let path = match value {
        None => dirs::home_dir().ok_or_else(super::resource_unavailable)?,
        Some(value) if value.starts_with("file:") => {
            let url =
                reqwest::Url::parse(value).map_err(|_| super::invalid_request("文件链接无效。"))?;
            if url.host_str().is_some_and(|host| host != "localhost")
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(super::invalid_request("文件链接必须指向当前 Host。"));
            }
            url.to_file_path()
                .map_err(|_| super::invalid_request("文件链接无效。"))?
        }
        Some(value) => PathBuf::from(value),
    };
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(super::invalid_request("请输入 Host 上的绝对路径。"));
    }
    let path = std::fs::canonicalize(path).map_err(error)?;
    path_text(&path)?;
    Ok(path)
}

/// canonicalize 的结果不是永久授权。逐级以目录句柄打开且拒绝再次跟随链接，校验后替换的链接无法改变读取目标。
/// NONBLOCK 避免被替换成 FIFO 时阻塞；最终文件类型由句柄核验。仅支持正式 Host 现有 Unix 平台。
pub(super) fn open_resolved(path: &Path, directory: bool) -> Result<File, RuntimeErrorInfo> {
    if !path.is_absolute() {
        return Err(super::invalid_request("文件路径必须为绝对路径。"));
    }
    let mut fd = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(error)?;
    let parts: Vec<_> = path
        .components()
        .filter(|c| !matches!(c, Component::RootDir))
        .collect();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err(super::invalid_request("文件路径不是规范路径。"));
        };
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        if directory || index + 1 < parts.len() {
            flags |= OFlags::DIRECTORY;
        }
        fd = openat(&fd, *name, flags, Mode::empty()).map_err(error)?;
    }
    let file = File::from(fd);
    let metadata = file.metadata().map_err(error)?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(super::invalid_request("请选择普通文件或目录。"));
    }
    Ok(file)
}

pub(super) fn list(
    directory: &Path,
    boundary: Option<&Path>,
    hidden: bool,
    generated: bool,
) -> Result<ListHostFilesResult, RuntimeErrorInfo> {
    let file = open_resolved(directory, true)?;
    let reader = Dir::read_from(&file).map_err(error)?;
    let mut result = ListHostFilesResult {
        path: path_text(directory)?,
        parent_path: directory.parent().map(path_text).transpose()?,
        entries: vec![],
        truncated: false,
        skipped_entries: 0,
    };
    let mut scanned = 0;
    for entry in reader {
        let entry = entry.map_err(error)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if scanned == MAX_DIRECTORY_ENTRIES {
            result.truncated = true;
            break;
        }
        scanned += 1;
        let Ok(name) = std::str::from_utf8(bytes) else {
            result.skipped_entries += 1;
            continue;
        };
        if (!hidden && name.starts_with('.')) || (!generated && is_generated(name)) {
            continue;
        }
        let candidate = directory.join(name);
        let known_directory = std::fs::metadata(&candidate).is_ok_and(|m| m.is_dir());
        let is_symbolic_link =
            std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink());
        let resolved = std::fs::canonicalize(&candidate);
        let (kind, state, size) = match resolved {
            Ok(path) if boundary.is_some_and(|root| !path.starts_with(root)) => (
                SessionResourceEntryKind::File,
                SessionResourceEntryState::OutsideRoot,
                None,
            ),
            Ok(path) => match open_resolved(&path, true)
                .or_else(|_| open_resolved(&path, false))
                .and_then(|f| f.metadata().map_err(error))
            {
                Ok(m) if m.is_dir() => (
                    SessionResourceEntryKind::Directory,
                    SessionResourceEntryState::Available,
                    None,
                ),
                Ok(m) => (
                    SessionResourceEntryKind::File,
                    SessionResourceEntryState::Available,
                    Some(m.len()),
                ),
                Err(_) => (
                    if known_directory {
                        SessionResourceEntryKind::Directory
                    } else {
                        SessionResourceEntryKind::File
                    },
                    SessionResourceEntryState::Unsupported,
                    None,
                ),
            },
            Err(_) => (
                if known_directory {
                    SessionResourceEntryKind::Directory
                } else {
                    SessionResourceEntryKind::File
                },
                SessionResourceEntryState::Unsupported,
                None,
            ),
        };
        result.entries.push(HostFileEntry {
            path: path_text(&candidate)?,
            display_name: name.to_owned(),
            kind,
            state,
            size_bytes: size,
            is_symbolic_link,
        });
    }
    result.entries.sort_by(|a, b| {
        (b.kind == SessionResourceEntryKind::Directory)
            .cmp(&(a.kind == SessionResourceEntryKind::Directory))
            .then_with(|| {
                a.display_name
                    .to_lowercase()
                    .cmp(&b.display_name.to_lowercase())
            })
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    Ok(result)
}

pub(super) fn is_generated(name: &str) -> bool {
    GENERATED.contains(&name)
}

/// 读取预算约束真实读取量，不依赖开始时的 metadata；使用同一个句柄嗅探和读取，避免路径替换。
pub(super) fn preview(path: &Path) -> Result<PreviewSessionResourceFileResult, RuntimeErrorInfo> {
    let mut file = open_resolved(path, false)?;
    let mut head = [0; 8192];
    let length = file.read(&mut head).map_err(error)?;
    file.seek(SeekFrom::Start(0)).map_err(error)?;
    let mime = infer::get(&head[..length])
        .map(|m| m.mime_type())
        .unwrap_or("application/octet-stream");
    let image = matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    );
    let pdf = mime == "application/pdf";
    let bytes = read_bounded(
        file,
        if image || pdf {
            MEDIA_LIMIT
        } else {
            TEXT_LIMIT
        },
    )?;
    let size_bytes = bytes.len() as u64;
    if image {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(error)?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        limits.max_alloc = Some(160_000_000);
        reader.limits(limits);
        let decoded = reader.decode().map_err(|_| {
            RuntimeErrorInfo::new(
                RuntimeErrorCode::ResourceNotPreviewable,
                "图片无效或超过尺寸限制。",
            )
        })?;
        if u64::from(decoded.width()) * u64::from(decoded.height()) > 40_000_000 {
            return Err(too_large());
        }
    }
    if image || pdf {
        return Ok(PreviewSessionResourceFileResult {
            kind: if image {
                SessionResourcePreviewKind::Image
            } else {
                SessionResourcePreviewKind::Pdf
            },
            media_type: mime.into(),
            size_bytes,
            text: None,
            data_base64: Some(STANDARD.encode(bytes)),
        });
    }
    if bytes.contains(&0) {
        return Err(RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceNotPreviewable,
            "此文件不能作为文本预览，请下载查看。",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        RuntimeErrorInfo::new(
            RuntimeErrorCode::ResourceNotPreviewable,
            "此文件不是 UTF-8 文本，请下载查看。",
        )
    })?;
    let mime = super::preview_media_type(path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
        .filter(|m| m.starts_with("text/") || m.starts_with("application/json"))
        .unwrap_or("text/plain; charset=utf-8");
    Ok(PreviewSessionResourceFileResult {
        kind: SessionResourcePreviewKind::Text,
        media_type: mime.into(),
        size_bytes,
        text: Some(text),
        data_base64: None,
    })
}

pub(super) fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, RuntimeErrorInfo> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

fn too_large() -> RuntimeErrorInfo {
    RuntimeErrorInfo::new(
        RuntimeErrorCode::ResourceTooLarge,
        "文件超过预览限制，请下载查看。",
    )
}

#[cfg(test)]
mod tests;
