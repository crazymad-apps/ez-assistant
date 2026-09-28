//! 归档只在新建的用户暂存目录内展开，完整静态准入后才把目录交给安装流程。
//! 包内链接最后创建；解包时任何路径都不会经过包内链接写入。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;
use zip::{CompressionMethod, ZipArchive};

use super::{PackageError, PluginManifest, Result};

const ARCHIVE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const FILE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
const ENTRY_LIMIT: usize = 100_000;
const DESCRIPTOR_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchiveFormat {
    Zip,
    TarGzip,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EntryKind {
    File,
    Directory,
    Link,
}

#[derive(Debug)]
pub(super) struct EntryInfo {
    pub(super) kind: EntryKind,
    pub(super) mode: u32,
    pub(super) size: u64,
    link: Option<String>,
}

pub(super) type EntryIndex = BTreeMap<String, EntryInfo>;

/// 准入完成的暂存包；调用方必须在 DB 发布成功后才可移动内容到生效目录。
/// 返回的 TempDir 仍属于调用方，失败或放弃时显式 close 才能观察清理错误。
pub(crate) struct PreparedPackage {
    pub(crate) staging: TempDir,
    pub(crate) manifest: PluginManifest,
    pub(crate) sha256: String,
}

/// 使用调用方创建的私有用户暂存根，绝不按归档内容推断目标路径。
/// 此处同步 I/O 应由安装 owner 通过有并发上限的 blocking task 调用。
#[cfg(test)]
pub(crate) fn prepare_archive(
    archive: &Path,
    format: ArchiveFormat,
    staging_root: &Path,
) -> Result<PreparedPackage> {
    prepare_archive_with_cancel(archive, format, staging_root, &CancellationToken::new())
}

pub(crate) fn prepare_archive_with_cancel(
    archive: &Path,
    format: ArchiveFormat,
    staging_root: &Path,
    cancellation: &CancellationToken,
) -> Result<PreparedPackage> {
    check_cancel(cancellation)?;
    let metadata = fs::symlink_metadata(archive)?;
    if !metadata.file_type().is_file() || metadata.len() > ARCHIVE_LIMIT {
        return Err(PackageError::TooLarge);
    }
    let mut input = File::open(archive)?;
    let mut magic = [0; 4];
    input
        .read_exact(&mut magic)
        .map_err(|_| PackageError::Invalid)?;
    if match format {
        ArchiveFormat::Zip => magic != [0x50, 0x4b, 0x03, 0x04],
        ArchiveFormat::TarGzip => magic[..2] != [0x1f, 0x8b],
    } {
        return Err(PackageError::Invalid);
    }
    let mut hasher = Sha256::new();
    let mut file = File::open(archive)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancel(cancellation)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.write_all(&buffer[..count])?;
    }
    let sha256 = format!("{:x}", hasher.finalize());
    let staging = Builder::new().prefix("package-").tempdir_in(staging_root)?;
    let checked = (|| {
        let root = staging.path();
        let mut index = EntryIndex::new();
        let mut total = 0_u64;
        match format {
            ArchiveFormat::Zip => scan_zip(archive, root, &mut index, &mut total, cancellation)?,
            ArchiveFormat::TarGzip => {
                scan_tar(archive, root, &mut index, &mut total, cancellation)?
            }
        }
        validate_index(&index)?;
        let manifest =
            PluginManifest::parse(&read_bounded(&root.join("plugin.json"), DESCRIPTOR_LIMIT)?)?;
        manifest.validate_entries(root, &index)?;
        if !manifest.supports_host() {
            return Err(PackageError::Invalid);
        }
        let supported = manifest.platforms.iter().any(|p| {
            let os = match std::env::consts::OS {
                "macos" => "macos",
                "windows" => "windows",
                "linux" => "linux",
                _ => return false,
            };
            let arch = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                _ => return false,
            };
            p.os == os && p.arch == arch
        });
        if !supported {
            return Err(PackageError::Invalid);
        }
        create_links(root, &index)?;
        // Staging is not published until every extracted file and directory has been
        // synced. Syncing each file while writing it serializes every directory
        // update with disk flushes and makes large packages take minutes.
        sync_staging(root, cancellation)?;
        Ok(manifest)
    })();
    match checked {
        Ok(manifest) => Ok(PreparedPackage {
            staging,
            manifest,
            sha256,
        }),
        Err(error) => {
            // 暂存目录已经不再被文件句柄使用；清理失败比原始包错误更需要优先暴露。
            staging.close()?;
            Err(error)
        }
    }
}

fn register(
    index: &mut EntryIndex,
    raw: &str,
    kind: EntryKind,
    mode: u32,
    size: u64,
) -> Result<String> {
    let path = if kind == EntryKind::Directory {
        raw.strip_suffix('/').unwrap_or(raw)
    } else {
        raw
    };
    if path.is_empty()
        || path.len() > 1024
        || path.nfc().collect::<String>() != path
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains('\u{fffd}')
        || path.as_bytes().get(1) == Some(&b':')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || path != "plugin.json"
            && !matches!(
                path.split('/').next(),
                Some("skills" | "mcp" | "runtimes" | "resources" | "licenses")
            )
        || mode & 0o6000 != 0
        || size > FILE_LIMIT
        || kind == EntryKind::Directory && size != 0
        || index.len() >= ENTRY_LIMIT
        || index.contains_key(path)
    {
        return Err(PackageError::Invalid);
    }
    index.insert(
        path.to_owned(),
        EntryInfo {
            kind,
            mode,
            size,
            link: None,
        },
    );
    Ok(path.to_owned())
}

fn scan_zip(
    archive: &Path,
    root: &Path,
    index: &mut EntryIndex,
    total: &mut u64,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut zip = ZipArchive::new(File::open(archive)?).map_err(|_| PackageError::Invalid)?;
    let mut central = File::open(archive)?;
    if zip.len() > ENTRY_LIMIT {
        return Err(PackageError::TooLarge);
    }
    let mut spans = Vec::with_capacity(zip.len());
    for number in 0..zip.len() {
        check_cancel(cancellation)?;
        let mut entry = zip.by_index(number).map_err(|_| PackageError::Invalid)?;
        if entry.encrypted()
            || !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(PackageError::Invalid);
        }
        validate_zip_name_encoding(&mut central, entry.central_header_start())?;
        let name = entry.name();
        let mode = entry.unix_mode().unwrap_or(0o100644);
        let file_type = mode & 0o170000;
        let kind = if file_type == 0o120000 {
            EntryKind::Link
        } else if entry.is_dir() || file_type == 0o040000 {
            EntryKind::Directory
        } else if matches!(file_type, 0 | 0o100000) {
            EntryKind::File
        } else {
            return Err(PackageError::Invalid);
        };
        if entry.is_dir() != (kind == EntryKind::Directory) {
            return Err(PackageError::Invalid);
        }
        let size = entry.size();
        let path = register(index, name, kind, mode, size)?;
        let start = entry.data_start().ok_or(PackageError::Invalid)?;
        let end = start
            .checked_add(entry.compressed_size())
            .ok_or(PackageError::Invalid)?;
        if end > fs::metadata(archive)?.len() {
            return Err(PackageError::Invalid);
        }
        spans.push((start, end));
        if kind == EntryKind::Directory {
            fs::create_dir_all(root.join(&path))?;
            continue;
        }
        let mut digest = crc32fast::Hasher::new();
        if kind == EntryKind::Link {
            let mut bytes = Vec::new();
            copy_bounded(
                &mut entry,
                &mut bytes,
                size,
                total,
                &mut digest,
                cancellation,
            )?;
            if bytes.len() > 1024 {
                return Err(PackageError::Invalid);
            }
            let link = String::from_utf8(bytes).map_err(|_| PackageError::Invalid)?;
            index.get_mut(&path).ok_or(PackageError::Invalid)?.link = Some(link);
        } else {
            write_entry(
                &root.join(&path),
                &mut entry,
                size,
                total,
                &mut digest,
                mode,
                cancellation,
            )?;
        }
        if digest.finalize() != entry.crc32() {
            return Err(PackageError::Invalid);
        }
    }
    spans.sort_unstable();
    if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(PackageError::Invalid);
    }
    Ok(())
}

/// 与 C01 一致：非 ASCII 原始名称必须声明 UTF-8 或带 CRC 匹配的 Unicode Path 扩展。
/// zip crate 会据有效扩展解码 name；这里只复核原始 central directory 标志与来源。
fn validate_zip_name_encoding(reader: &mut File, start: u64) -> Result<()> {
    reader.seek(SeekFrom::Start(start))?;
    let mut header = [0_u8; 46];
    reader
        .read_exact(&mut header)
        .map_err(|_| PackageError::Invalid)?;
    if header[..4] != [0x50, 0x4b, 0x01, 0x02] {
        return Err(PackageError::Invalid);
    }
    let flags = u16::from_le_bytes([header[8], header[9]]);
    let name_len = u16::from_le_bytes([header[28], header[29]]) as usize;
    let extra_len = u16::from_le_bytes([header[30], header[31]]) as usize;
    let mut raw = vec![0_u8; name_len];
    reader
        .read_exact(&mut raw)
        .map_err(|_| PackageError::Invalid)?;
    if raw.iter().all(u8::is_ascii) || flags & 0x800 != 0 {
        return Ok(());
    }
    let mut extras = vec![0_u8; extra_len];
    reader
        .read_exact(&mut extras)
        .map_err(|_| PackageError::Invalid)?;
    let mut cursor = extras.as_slice();
    while cursor.len() >= 4 {
        let id = u16::from_le_bytes([cursor[0], cursor[1]]);
        let size = u16::from_le_bytes([cursor[2], cursor[3]]) as usize;
        cursor = &cursor[4..];
        if cursor.len() < size {
            return Err(PackageError::Invalid);
        }
        if id == 0x7075
            && size >= 5
            && cursor[0] == 1
            && u32::from_le_bytes([cursor[1], cursor[2], cursor[3], cursor[4]])
                == crc32fast::hash(&raw)
        {
            return Ok(());
        }
        cursor = &cursor[size..];
    }
    Err(PackageError::Invalid)
}

fn scan_tar(
    archive: &Path,
    root: &Path,
    index: &mut EntryIndex,
    total: &mut u64,
    cancellation: &CancellationToken,
) -> Result<()> {
    preflight_tar(archive, cancellation)?;
    let decoder = GzDecoder::new(File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().map_err(|_| PackageError::Invalid)? {
        check_cancel(cancellation)?;
        let mut entry = entry.map_err(|_| PackageError::Invalid)?;
        if let Some(extensions) = entry.pax_extensions().map_err(|_| PackageError::Invalid)? {
            for extension in extensions {
                let extension = extension.map_err(|_| PackageError::Invalid)?;
                let key = extension.key().map_err(|_| PackageError::Invalid)?;
                if !matches!(
                    key,
                    "global" | "path" | "linkpath" | "mtime" | "atime" | "ctime" | "size"
                ) {
                    return Err(PackageError::Invalid);
                }
            }
        }
        let kind = match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => EntryKind::File,
            tar::EntryType::Directory => EntryKind::Directory,
            tar::EntryType::Symlink => EntryKind::Link,
            _ => return Err(PackageError::Invalid),
        };
        let name = std::str::from_utf8(entry.path_bytes().as_ref())
            .map_err(|_| PackageError::Invalid)?
            .to_owned();
        let mode = entry.header().mode().map_err(|_| PackageError::Invalid)?;
        let size = entry.size();
        let path = register(index, &name, kind, mode, size)?;
        if kind == EntryKind::Directory {
            fs::create_dir_all(root.join(&path))?;
        } else if kind == EntryKind::Link {
            let raw = entry.link_name_bytes().ok_or(PackageError::Invalid)?;
            let link = std::str::from_utf8(raw.as_ref()).map_err(|_| PackageError::Invalid)?;
            index.get_mut(&path).ok_or(PackageError::Invalid)?.link = Some(link.to_owned());
            if size != 0 {
                return Err(PackageError::Invalid);
            }
        } else {
            write_entry(
                &root.join(&path),
                &mut entry,
                size,
                total,
                &mut crc32fast::Hasher::new(),
                mode,
                cancellation,
            )?;
        }
    }
    Ok(())
}

/// 先用 raw iterator 逐头读取元数据，防止 tar 库在展开 PAX/GNU 名称前分配无界缓冲。
/// 解码总量与压缩比按 header 声明保守计数；正式扫描仍逐文件核对实际字节。
fn preflight_tar(archive: &Path, cancellation: &CancellationToken) -> Result<()> {
    let compressed = fs::metadata(archive)?.len().max(1);
    let limit = TOTAL_LIMIT + ENTRY_LIMIT as u64 * 512;
    let mut decoded = 0_u64;
    let mut count = 0_usize;
    let decoder = GzDecoder::new(File::open(archive)?);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().map_err(|_| PackageError::Invalid)?.raw(true) {
        check_cancel(cancellation)?;
        let entry = entry.map_err(|_| PackageError::Invalid)?;
        count += 1;
        if count > ENTRY_LIMIT {
            return Err(PackageError::TooLarge);
        }
        let kind = entry.header().entry_type();
        let size = entry.size();
        let metadata = kind.is_pax_local_extensions()
            || kind.is_pax_global_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink();
        if metadata && size > DESCRIPTOR_LIMIT {
            return Err(PackageError::TooLarge);
        }
        if !metadata && size > FILE_LIMIT {
            return Err(PackageError::TooLarge);
        }
        decoded = decoded
            .checked_add(512)
            .and_then(|total| total.checked_add(size))
            .and_then(|total| total.checked_add((512 - size % 512) % 512))
            .ok_or(PackageError::TooLarge)?;
        if decoded > limit || decoded > compressed.saturating_mul(100_000) {
            return Err(PackageError::TooLarge);
        }
    }
    Ok(())
}

fn write_entry(
    target: &Path,
    input: &mut impl Read,
    size: u64,
    total: &mut u64,
    crc: &mut crc32fast::Hasher,
    mode: u32,
    cancellation: &CancellationToken,
) -> Result<()> {
    #[cfg(not(unix))]
    let _ = mode;
    fs::create_dir_all(target.parent().ok_or(PackageError::Invalid)?)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    copy_bounded(input, &mut output, size, total, crc, cancellation)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

fn sync_staging(root: &Path, cancellation: &CancellationToken) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        check_cancel(cancellation)?;
        directories.push(directory.clone());
        for entry in fs::read_dir(&directory)? {
            check_cancel(cancellation)?;
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                crate::platform::sync_file_at(&entry.path())?;
            }
        }
    }
    for directory in directories.into_iter().rev() {
        check_cancel(cancellation)?;
        crate::platform::sync_directory(&directory)?;
    }
    Ok(())
}

fn copy_bounded(
    input: &mut impl Read,
    output: &mut impl Write,
    expected: u64,
    total: &mut u64,
    crc: &mut crc32fast::Hasher,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut actual = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancel(cancellation)?;
        let count = input.read(&mut buffer).map_err(|_| PackageError::Invalid)?;
        if count == 0 {
            break;
        }
        actual = actual
            .checked_add(count as u64)
            .ok_or(PackageError::TooLarge)?;
        *total = total
            .checked_add(count as u64)
            .ok_or(PackageError::TooLarge)?;
        if actual > expected || actual > FILE_LIMIT || *total > TOTAL_LIMIT {
            return Err(PackageError::TooLarge);
        }
        crc.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if actual != expected {
        return Err(PackageError::Invalid);
    }
    Ok(())
}

fn check_cancel(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(PackageError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    if fs::metadata(path)?.len() > limit {
        return Err(PackageError::TooLarge);
    }
    fs::read(path).map_err(Into::into)
}

fn validate_index(index: &EntryIndex) -> Result<()> {
    let mut folded = BTreeSet::new();
    for (path, entry) in index {
        if !folded.insert(path.to_lowercase()) {
            return Err(PackageError::Invalid);
        }
        let mut parent = PathBuf::from(path);
        while parent.pop() {
            if let Some(parent) = parent.to_str()
                && index
                    .get(parent)
                    .is_some_and(|item| item.kind != EntryKind::Directory)
            {
                return Err(PackageError::Invalid);
            }
        }
        if entry.kind == EntryKind::Link {
            resolve_link(path, index, &mut BTreeSet::new())?;
        }
    }
    Ok(())
}

fn resolve_link<'a>(
    path: &str,
    index: &'a EntryIndex,
    seen: &mut BTreeSet<String>,
) -> Result<&'a EntryInfo> {
    if !seen.insert(path.to_owned()) {
        return Err(PackageError::Invalid);
    }
    let entry = index.get(path).ok_or(PackageError::Invalid)?;
    if entry.kind != EntryKind::Link {
        return Ok(entry);
    }
    let link = entry.link.as_deref().ok_or(PackageError::Invalid)?;
    if link.is_empty()
        || link.len() > 1024
        || link.starts_with('/')
        || link.contains('\\')
        || link.contains('\0')
        || link.contains('\u{fffd}')
        || link.as_bytes().get(1) == Some(&b':')
    {
        return Err(PackageError::Invalid);
    }
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    for part in link.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or(PackageError::Invalid)?;
            }
            _ => parts.push(part),
        }
    }
    let resolved = parts.join("/");
    resolve_link(&resolved, index, seen)
}

fn create_links(root: &Path, index: &EntryIndex) -> Result<()> {
    for (path, entry) in index {
        if entry.kind != EntryKind::Link {
            continue;
        }
        let link = entry.link.as_deref().ok_or(PackageError::Invalid)?;
        let target = root.join(path);
        fs::create_dir_all(target.parent().ok_or(PackageError::Invalid)?)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, target)?;
        #[cfg(windows)]
        {
            let _ = (link, target);
            return Err(PackageError::Invalid);
        }
    }
    Ok(())
}
