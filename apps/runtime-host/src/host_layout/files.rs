//! 整套布局备份的文件证据与私有原子写入。复制不跟随链接，不扫描外部工作区。

use super::{Error, Result};
use crate::{config_source::prepare_private_directory, platform};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) enum Entry {
    Directory,
    File { size: u64, sha256: String },
    Link { target: PathBuf },
}

pub(super) fn bytes_entry(bytes: &[u8]) -> Entry {
    Entry::File {
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}

pub(super) fn inspect(path: &Path) -> Result<Entry> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(Entry::Link {
            target: fs::read_link(path)?,
        });
    }
    if platform::is_reparse_or_link(&metadata) {
        return Err(Error::Conflict);
    }
    if metadata.is_dir() {
        return Ok(Entry::Directory);
    }
    if !metadata.is_file() {
        return Err(Error::Conflict);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    platform::apply_read_no_follow(&mut options);
    let mut file = options.open(path)?;
    if !platform::same_file_identity(&metadata, &file.metadata()?) {
        return Err(Error::Conflict);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        size += count as u64;
    }
    if size != metadata.len() {
        return Err(Error::Conflict);
    }
    Ok(Entry::File {
        size,
        sha256: format!("{:x}", digest.finalize()),
    })
}

pub(super) fn inventory(
    root: &Path,
    relative: &Path,
    output: &mut BTreeMap<PathBuf, Entry>,
) -> Result<()> {
    let entry = inspect(&root.join(relative))?;
    let directory = entry == Entry::Directory;
    output.insert(relative.to_owned(), entry);
    if directory {
        for item in fs::read_dir(root.join(relative))? {
            inventory(root, &relative.join(item?.file_name()), output)?;
        }
    }
    Ok(())
}

/// 目标必须不存在；源与复制件逐项回读，文件的独立 inode 不依赖原布局生命周期。
pub(super) fn copy_entry(source: &Path, target: &Path, entry: &Entry) -> Result<()> {
    if exists(target)? {
        return Err(Error::Conflict);
    }
    prepare_private_directory(target.parent().ok_or(Error::Conflict)?)?;
    match entry {
        Entry::Directory => prepare_private_directory(target)?,
        Entry::File { .. } => {
            let mut options = OpenOptions::new();
            options.read(true);
            platform::apply_read_no_follow(&mut options);
            let mut input = options.open(source)?;
            let mut output = private_file(target)?;
            std::io::copy(&mut input, &mut output)?;
            // 保留可执行/只读位；备份父目录本身为私有，内容不会因此对其他 OS 用户开放。
            output.set_permissions(input.metadata()?.permissions())?;
            output.sync_all()?;
        }
        Entry::Link {
            target: destination,
        } => platform::copy_layout_link(destination, target)?,
    }
    if &inspect(target)? != entry || &inspect(source)? != entry {
        return Err(Error::Conflict);
    }
    Ok(())
}

pub(super) fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn private_file(path: &Path) -> Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    platform::apply_private_open_options(&mut options);
    Ok(options.open(path)?)
}

/// 文件内容必须已与备份原件或确定性转换结果核对。只替换调用方拥有的目标；先 fsync 再 rename。
pub(super) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or(Error::Conflict)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    platform::tighten_private_file(temporary.as_file())?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    platform::sync_directory(parent)?;
    if inspect(path)? != bytes_entry(bytes) {
        return Err(Error::Conflict);
    }
    Ok(())
}
