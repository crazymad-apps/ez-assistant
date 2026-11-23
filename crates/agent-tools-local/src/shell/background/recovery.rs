//! 只读受管目录中的有限身份文件，补清后删除该次运行的已知文件。

use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use super::identity::{self, Identity};

pub const BACKGROUND_SHELL_DIRECTORY: &str = "background-shell";
pub(super) const OUTPUT_FILE: &str = "output.log";
const IDENTITY_FILE: &str = "identity.json";
const IDENTITY_STAGING: &str = "identity.tmp";
const START_MARKER: &str = "starting";

/// Host 持有 Runtime Home 独占锁且尚未启动任何用户域时调用。逐项返回补清失败原因。
pub fn recover_background_shells(users: &Path) -> io::Result<Vec<io::Error>> {
    let mut errors = Vec::new();
    for user in entries(users)? {
        let user_path = match user.and_then(|entry| {
            let path = entry.path();
            Ok(is_plain_directory(&fs::symlink_metadata(&path)?).then_some(path))
        }) {
            Ok(Some(path)) => path,
            Ok(None) => continue,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let root = user_path.join(BACKGROUND_SHELL_DIRECTORY);
        match entries(&root) {
            Ok(runs) => {
                for run in runs {
                    let result = run.and_then(|entry| recover_run(&entry.path()));
                    if let Err(error) = result {
                        errors.push(error);
                    }
                }
            }
            Err(error) => errors.push(error),
        }
    }
    Ok(errors)
}

fn recover_run(directory: &Path) -> io::Result<()> {
    if !directory
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(valid_id)
    {
        return Ok(());
    }
    if !is_plain_directory(&fs::symlink_metadata(directory)?) {
        return Err(io::Error::other("unsafe background run directory"));
    }
    let record = match open_record(&directory.join(IDENTITY_FILE))? {
        Some(file) => Some(file),
        // fsync 与 rename 之间发生故障时，完整暂存记录仍可证明同一个主 PID。
        None => open_record(&directory.join(IDENTITY_STAGING))?,
    };
    if record.is_none() && open_record(&directory.join(START_MARKER))?.is_some() {
        // 睡眠、崩溃或强制终止可能发生在 spawn 与身份落盘之间。缺证据时保留名额。
        return Err(io::Error::other(
            "managed shell launch identity is unconfirmed",
        ));
    }
    if let Some(file) = record {
        let mut data = Vec::new();
        file.take(4_097).read_to_end(&mut data)?;
        if data.len() > 4_096 {
            return Err(io::Error::other("background identity record is too large"));
        }
        let record = serde_json::from_slice::<Identity>(&data)?;
        let scheduled = directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("scheduled_"));
        identity::terminate_if_matching(&record, scheduled)?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if scheduled {
            identity::verify_group_exit(record.pid)?;
        }
    }
    // 没有记录的目录来自交接前崩溃或只剩临时日志；仅删除确定属于本能力的文件。
    clean_run_directory(directory)
}

pub(super) fn valid_id(name: &str) -> bool {
    name.strip_prefix("background_")
        .or_else(|| name.strip_prefix("scheduled_"))
        .is_some_and(|id| {
            id.len() == 12
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

pub(super) fn create_run_directory(root: &Path, id: &str) -> io::Result<PathBuf> {
    let builder = DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(root) {
        Ok(()) => {}
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && is_plain_directory(&fs::symlink_metadata(root)?) => {}
        Err(error) => return Err(error),
    }
    let directory = root.join(id);
    builder.create(&directory)?;
    Ok(directory)
}

pub(super) fn mark_starting(directory: &Path) -> io::Result<()> {
    let marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(START_MARKER))?;
    marker.sync_all()?;
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()?;
        if let Some(root) = directory.parent() {
            File::open(root)?.sync_all()?;
            if let Some(user) = root.parent() {
                File::open(user)?.sync_all()?;
            }
        }
    }
    Ok(())
}

pub(super) fn record_identity(directory: &Path, record: &Identity) -> io::Result<()> {
    let source = directory.join(IDENTITY_STAGING);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&source)?;
    file.write_all(&serde_json::to_vec(record)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(source, directory.join(IDENTITY_FILE))?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) fn clean_run_directory(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if is_plain_directory(&metadata) => {}
        Ok(_) => return Err(io::Error::other("unsafe background run directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    // 最后删除身份记录，日志删除失败时下次启动仍能找到关联文件。
    for name in [OUTPUT_FILE, START_MARKER, IDENTITY_STAGING, IDENTITY_FILE] {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !is_link(&metadata) => fs::remove_file(path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("unsafe background run file")),
        }
    }
    fs::remove_dir(directory)
}

fn entries(directory: &Path) -> io::Result<impl Iterator<Item = io::Result<fs::DirEntry>>> {
    let entries = match fs::symlink_metadata(directory) {
        Ok(metadata) if is_plain_directory(&metadata) => Some(fs::read_dir(directory)?),
        Ok(_) => return Err(io::Error::other("unsafe background storage directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(entries.into_iter().flatten())
}

fn is_plain_directory(metadata: &Metadata) -> bool {
    metadata.is_dir() && !is_link(metadata)
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

fn open_record(path: &Path) -> io::Result<Option<File>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {}
        Ok(_) => return Err(io::Error::other("unsafe background identity file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    #[cfg(target_os = "linux")]
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)?;
    #[cfg(not(target_os = "linux"))]
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(io::Error::other("unsafe background identity handle"));
    }
    Ok(Some(file))
}
