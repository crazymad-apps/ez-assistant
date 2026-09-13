//! Host 浏览路径的 Windows 打开边界。逐级句柄阻止目录重命名，拒绝全部 reparse tag。

use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx,
};

/// 调用方在所有依赖路径的操作完成前保留 guards；纯句柄读取可以只保留最终 file。
pub(crate) struct OpenedPath {
    pub(crate) file: File,
    _parents: Vec<File>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// 只接受本机盘符；拒绝 UNC、设备命名空间、相对盘符和路径穿越，先于任何文件系统访问。
pub(crate) fn local_path(path: &Path) -> io::Result<PathBuf> {
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => return Err(invalid("UNC and device paths are not supported")),
        },
        _ => return Err(invalid("an absolute local drive path is required")),
    };
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(invalid("an absolute local drive path is required"));
    }
    let mut normalized = PathBuf::from(format!("\\\\?\\{}:\\", char::from(drive)));
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(invalid("path must not contain traversal components"));
        };
        // 冒号会选择 NTFS alternate stream；本入口只允许普通文件与目录。
        if name
            .as_encoded_bytes()
            .iter()
            .any(|b| matches!(b, 0 | b':'))
        {
            return Err(invalid("alternate streams and NUL are not supported"));
        }
        normalized.push(name);
    }
    Ok(normalized)
}

fn open_component(path: &Path, directory: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        // 目录不共享 WRITE/DELETE：阻止重命名及原地改写 reparse tag；普通文件允许内容写入，
        // 读取仍使用同一句柄和已有大小预算，不宣称提供不可变内容快照。
        .share_mode(if directory {
            FILE_SHARE_READ
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if super::is_reparse_or_link(&metadata) {
        return Err(invalid("reparse points are not supported"));
    }
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(invalid("a regular file or directory is required"));
    }
    Ok(file)
}

pub(crate) fn open(path: &Path, directory: bool) -> io::Result<OpenedPath> {
    let path = local_path(path)?;
    let mut parts = path.components();
    let mut current = PathBuf::new();
    // local_path 已建立 Prefix + RootDir 不变量。
    current.push(
        parts
            .next()
            .ok_or_else(|| invalid("missing drive"))?
            .as_os_str(),
    );
    current.push(
        parts
            .next()
            .ok_or_else(|| invalid("missing root"))?
            .as_os_str(),
    );
    let names: Vec<_> = parts.collect();
    let mut parents = Vec::with_capacity(names.len());
    let mut file = open_component(&current, directory || !names.is_empty())?;
    for (index, name) in names.iter().enumerate() {
        parents.push(file);
        current.push(name.as_os_str());
        file = open_component(&current, directory || index + 1 < names.len())?;
    }
    // 从仍被固定的路径再次打开，核验卷号和完整 128 位 ID，不依赖长度/时间戳启发式。
    let verified = open_component(&path, directory)?;
    if identity(&file)? != identity(&verified)? {
        return Err(invalid("file identity changed while opening"));
    }
    Ok(OpenedPath {
        file,
        _parents: parents,
    })
}

#[allow(unsafe_code)]
pub(crate) fn identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: 借用的 File 保持有效；info 是正确对齐、足够大小的输出结构，不跨调用保留指针。
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

pub(crate) fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let normalized = local_path(path)?;
    let opened = open(&normalized, true).or_else(|_| open(&normalized, false))?;
    let canonical = std::fs::canonicalize(&normalized)?;
    let verified = open(&canonical, opened.file.metadata()?.is_dir())?;
    if identity(&opened.file)? != identity(&verified.file)? {
        return Err(invalid("canonical file identity changed"));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_junction_before_canonicalization_and_open() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let junction = root.path().join("junction");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("private.txt"), b"outside").unwrap();
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(result.status.success(), "could not create junction fixture");
        assert!(open(&junction, true).is_err());
        assert!(open(&junction.join("private.txt"), false).is_err());
        assert!(canonicalize(&junction.join("private.txt")).is_err());
        // 只移除联接本身，不对目标做递归操作。
        std::fs::remove_dir(junction).unwrap();
        assert_eq!(
            std::fs::read(target.join("private.txt")).unwrap(),
            b"outside"
        );
    }

    #[test]
    fn rejects_network_device_and_relative_paths_before_io() {
        for path in [
            r"\\server\share\file",
            r"\\?\UNC\server\share\file",
            r"\\.\C:\file",
            r"C:relative",
            r"C:\a\..\b",
            r"C:\file:stream",
        ] {
            assert!(local_path(Path::new(path)).is_err(), "{path}");
        }
    }

    #[test]
    fn opens_long_unicode_paths_and_distinguishes_same_size_files() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("中文 空格目录".repeat(25));
        // 每个组件小于 255 字符，总路径超过传统 MAX_PATH。
        let directory = directory.join("长路径".repeat(30));
        std::fs::create_dir_all(local_path(&directory).unwrap()).unwrap();
        let first = local_path(&directory.join("first.txt")).unwrap();
        let second = local_path(&directory.join("second.txt")).unwrap();
        std::fs::write(&first, b"same").unwrap();
        std::fs::write(&second, b"same").unwrap();
        let first_open = open(&first, false).unwrap();
        let second_open = open(&second, false).unwrap();
        assert_ne!(
            identity(&first_open.file).unwrap(),
            identity(&second_open.file).unwrap()
        );
        assert_eq!(
            canonicalize(&first).unwrap(),
            std::fs::canonicalize(&first).unwrap()
        );
        // guards 保持期间无法替换父目录；丢弃后应恢复正常 rename。
        let moved = directory.with_file_name("moved");
        assert!(std::fs::rename(&directory, &moved).is_err());
        drop(first_open);
        drop(second_open);
        std::fs::rename(directory, moved).unwrap();
    }
}
