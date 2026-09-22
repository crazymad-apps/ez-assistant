//! v0.27.0 Host 目录升级；只用 host.toml 分派版本，最后发布版本才允许装配业务。
//! 调用方必须持有整个 Home 的实例锁。不可跨文件回滚的步骤以独立、已核验备份恢复。

mod files;
pub(crate) mod paths;
#[cfg(test)]
mod tests;
mod transform;

use crate::{
    config_source::{prepare_private_directory, read_config},
    host_configuration, platform,
    storage::migrations::layout as database,
};
use files::Entry;
use paths::Relocation;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const ROOTS: &[&str] = &[
    "data",
    "skills",
    "mcp.json",
    "mcp",
    "permissions.json",
    "recall-reference.key",
    "device",
    "backups/database",
    "backups/configuration",
];
const DATABASE: &str = "data/runtime.sqlite3";

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("Host layout conflicts with the verified backup; no further writes were performed")]
    Conflict,
    #[error("Host configuration is invalid or its layout version is unsupported")]
    Configuration,
    #[error("personal database is missing but managed data remains")]
    MissingDatabase,
    #[error("Host layout I/O failed")]
    Io(#[from] std::io::Error),
    #[error("Host layout directory could not be secured")]
    Directory(#[from] crate::config_source::RuntimeHomeError),
    #[error("Host layout backup manifest is invalid")]
    Json(#[from] serde_json::Error),
    #[error("Host layout database check or upgrade failed")]
    Database(#[from] crate::storage::migrations::MigrationError),
}

/// 不包含可变步骤；只证明备份来源、原件清单及确定性数据库目标。
#[derive(Deserialize, Serialize)]
struct Manifest {
    version: String,
    paths: Relocation,
    entries: BTreeMap<PathBuf, Entry>,
    database_projection: Option<serde_json::Value>,
}

pub(crate) fn personal_home(home: &Path) -> PathBuf {
    home.join("users/_personal")
}

/// 只读入口不创建 Home 或 host.toml；旧布局仍可预览原访问设置，写入必须先 upgrade。
pub(crate) fn read_configuration_path(home: &Path) -> Result<PathBuf> {
    let path = home.join(host_configuration::FILE);
    if files::exists(&path)? {
        check_current(&path, false)?;
        Ok(path)
    } else {
        Ok(home.join("config.toml"))
    }
}

fn check_current(path: &Path, repair: bool) -> Result<host_configuration::HostConfiguration> {
    match read_config(path, repair) {
        assistant_runtime::ConfigSourceLoad::Document(doc) => {
            host_configuration::parse(doc.contents()).map_err(|_| Error::Configuration)
        }
        _ => Err(Error::Configuration),
    }
}

/// 已持有实例锁，尚未开放监听。失败保留现场；再次调用仅根据固定备份和实际文件核验继续。
pub(crate) fn upgrade(home: &Path) -> Result<()> {
    for parent in ["backups", "users"] {
        let path = home.join(parent);
        if files::exists(&path)? && files::inspect(&path)? != Entry::Directory {
            return Err(Error::Conflict);
        }
    }
    let host = home.join(host_configuration::FILE);
    if files::exists(&host)? {
        check_current(&host, true)?;
        return Ok(());
    }
    let backup_root = home.join("backups/host-layout");
    let mut manifests = Vec::new();
    if files::exists(&backup_root)? {
        if files::inspect(&backup_root)? != Entry::Directory {
            return Err(Error::Conflict);
        }
        for candidate in fs::read_dir(&backup_root)? {
            let candidate = candidate?.path();
            if files::inspect(&candidate)? != Entry::Directory {
                return Err(Error::Conflict);
            }
            if files::exists(&candidate.join("manifest.json"))? {
                manifests.push(candidate);
            }
        }
    }
    let backup = match manifests.len() {
        0 => create_backup(home, &backup_root)?,
        1 => manifests.remove(0),
        _ => return Err(Error::Conflict),
    };
    eprintln!("runtime-host: layout upgrade backup: {}", backup.display());
    finish(home, &backup)
}

fn create_backup(home: &Path, root: &Path) -> Result<PathBuf> {
    let personal = personal_home(home);
    if files::exists(&personal)? {
        return Err(Error::Conflict);
    }
    for entry in fs::read_dir(home)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != "config.toml"
            && name != "run"
            && name != "users"
            && name != "backups"
            && !ROOTS.iter().any(|root| Path::new(root).as_os_str() == name)
        {
            eprintln!(
                "runtime-host: unrecognized root entry retained: {}",
                entry.path().display()
            );
        }
    }
    let mut entries = BTreeMap::new();
    let mut roots = Vec::new();
    for root in ROOTS {
        if files::exists(&home.join(root))? {
            let relative = PathBuf::from(root);
            // 根本身不能链接到外部用户数据；内部链接仅复制链接本身。
            if matches!(files::inspect(&home.join(root))?, Entry::Link { .. }) {
                return Err(Error::Conflict);
            }
            files::inventory(home, &relative, &mut entries)?;
            roots.push(relative);
        }
    }
    if files::exists(&home.join("config.toml"))? {
        match read_config(&home.join("config.toml"), true) {
            assistant_runtime::ConfigSourceLoad::Document(doc)
                if crate::access::validate_layout_configuration(doc.contents()).is_ok() => {}
            _ => return Err(Error::Configuration),
        }
        files::inventory(home, Path::new("config.toml"), &mut entries)?;
    }
    let source_database = home.join(DATABASE);
    let has_database = database::admit(&source_database)?.is_some();
    if !has_database
        && entries
            .keys()
            .any(|p| p.starts_with("data") && p != Path::new("data"))
    {
        return Err(Error::MissingDatabase);
    }
    let paths = Relocation {
        source: home.to_owned(),
        target: personal,
        roots,
    };
    prepare_private_directory(root)?;
    let backup = tempfile::Builder::new()
        .prefix("v0.27.0-")
        .tempdir_in(root)?
        .keep();
    let contents = backup.join("contents");
    prepare_private_directory(&contents)?;
    for (relative, entry) in &entries {
        if relative == Path::new(DATABASE) {
            database::copy(&source_database, &contents.join(relative))?;
        } else {
            files::copy_entry(&home.join(relative), &contents.join(relative), entry)?;
        }
    }
    if has_database {
        entries.insert(
            PathBuf::from(DATABASE),
            files::inspect(&contents.join(DATABASE))?,
        );
    }
    let database_projection = if has_database {
        Some(database::projected_evidence(
            &contents.join(DATABASE),
            &paths,
        )?)
    } else {
        None
    };
    let manifest = Manifest {
        version: host_configuration::LAYOUT_VERSION.into(),
        paths,
        entries,
        database_projection,
    };
    verify_backup(&contents, &manifest)?;
    files::write(
        &backup.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    platform::sync_directory(root)?;
    Ok(backup)
}

fn verify_backup(contents: &Path, manifest: &Manifest) -> Result<()> {
    let mut actual = BTreeMap::new();
    for root in &manifest.paths.roots {
        files::inventory(contents, root, &mut actual)?;
    }
    if manifest.entries.contains_key(Path::new("config.toml")) {
        files::inventory(contents, Path::new("config.toml"), &mut actual)?;
    }
    if actual != manifest.entries {
        return Err(Error::Conflict);
    }
    if let Some(expected) = &manifest.database_projection
        && database::projected_evidence(&contents.join(DATABASE), &manifest.paths)? != *expected
    {
        return Err(Error::Conflict);
    }
    Ok(())
}

fn finish(home: &Path, backup: &Path) -> Result<()> {
    finish_at_boundaries(home, backup, &mut || Ok(()))
}

/// 测试在每个可靠写入边界中断；生产回调无副作用，不保存额外步骤账本。
fn finish_at_boundaries(
    home: &Path,
    backup: &Path,
    boundary: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(backup.join("manifest.json"))?)?;
    if manifest.version != host_configuration::LAYOUT_VERSION
        || manifest.paths.source != home
        || manifest.paths.target != personal_home(home)
        || manifest
            .paths
            .roots
            .iter()
            .any(|r| !ROOTS.iter().any(|known| r == Path::new(known)))
    {
        return Err(Error::Conflict);
    }
    let contents = backup.join("contents");
    verify_backup(&contents, &manifest)?;
    let original = if manifest.entries.contains_key(Path::new("config.toml")) {
        Some(fs::read_to_string(contents.join("config.toml"))?)
    } else {
        None
    };
    let (host, personal) = transform::configurations(original.as_deref(), &manifest.paths)?;
    // 所有转换先计算并核对；遇到损坏/冲突不能边发现边搬下一项。
    let mut changed = BTreeMap::new();
    for (relative, entry) in &manifest.entries {
        if matches!(entry, Entry::File { size, .. } if *size <= 1024 * 1024)
            && transform::is_configuration(relative)
            && let Some(bytes) = transform::file(
                relative,
                &fs::read(contents.join(relative))?,
                &manifest.paths,
            )?
        {
            changed.insert(relative.clone(), bytes);
        }
    }
    for root in &manifest.paths.roots {
        let source = home.join(root);
        let target = manifest.paths.target.join(root);
        let source_exists = files::exists(&source)?;
        let target_exists = files::exists(&target)?;
        if source_exists == target_exists {
            return Err(Error::Conflict);
        }
        verify_tree(
            if source_exists {
                home
            } else {
                &manifest.paths.target
            },
            root,
            &manifest,
            &changed,
            !source_exists,
        )?;
    }
    verify_config(&home.join("config.toml"), original.as_deref(), true)?;
    verify_config(
        &manifest.paths.target.join("config.toml"),
        personal.as_deref(),
        true,
    )?;
    if original.is_some()
        && !files::exists(&home.join("config.toml"))?
        && !files::exists(&manifest.paths.target.join("config.toml"))?
    {
        return Err(Error::Conflict);
    }
    if files::exists(&manifest.paths.target)? {
        let mut existing = BTreeMap::new();
        files::inventory(&manifest.paths.target, Path::new(""), &mut existing)?;
        for relative in existing.keys().filter(|p| !p.as_os_str().is_empty()) {
            if relative != &PathBuf::from("config.toml")
                && !manifest
                    .paths
                    .roots
                    .iter()
                    .any(|root| relative.starts_with(root) || root.starts_with(relative))
            {
                return Err(Error::Conflict);
            }
        }
    }
    prepare_private_directory(&home.join("users"))?;
    prepare_private_directory(&manifest.paths.target)?;
    for root in &manifest.paths.roots {
        let source = home.join(root);
        let target = manifest.paths.target.join(root);
        if files::exists(&source)? {
            prepare_private_directory(target.parent().ok_or(Error::Conflict)?)?;
            fs::rename(&source, &target)?;
            platform::sync_directory(source.parent().ok_or(Error::Conflict)?)?;
            platform::sync_directory(target.parent().ok_or(Error::Conflict)?)?;
            boundary()?;
        }
    }
    for (relative, bytes) in &changed {
        let path = manifest.paths.target.join(relative);
        if files::inspect(&path)? != files::bytes_entry(bytes) {
            files::write(&path, bytes)?;
            boundary()?;
        }
    }
    if let Some(personal) = &personal {
        let path = manifest.paths.target.join("config.toml");
        if !files::exists(&path)? {
            files::write(&path, personal.as_bytes())?;
            boundary()?;
        }
    }
    if let Some(expected) = &manifest.database_projection {
        database::upgrade(&manifest.paths.target, &manifest.paths)?;
        boundary()?;
        database::verify(&manifest.paths.target.join(DATABASE), expected)?;
    }
    for root in &manifest.paths.roots {
        verify_tree(&manifest.paths.target, root, &manifest, &changed, true)?;
    }
    verify_config(
        &manifest.paths.target.join("config.toml"),
        personal.as_deref(),
        false,
    )?;
    if files::exists(&home.join("config.toml"))? {
        verify_config(&home.join("config.toml"), original.as_deref(), false)?;
        fs::remove_file(home.join("config.toml"))?;
        platform::sync_directory(home)?;
        boundary()?;
    }
    // 唯一提交点；之前业务服务不可见，也不能把旧个人 Skill 当公共资源。
    if files::exists(&home.join(host_configuration::FILE))? {
        return Err(Error::Conflict);
    }
    files::write(&home.join(host_configuration::FILE), host.as_bytes())?;
    boundary()?;
    Ok(())
}

fn verify_config(path: &Path, expected: Option<&str>, allow_missing: bool) -> Result<()> {
    if !files::exists(path)? {
        return if allow_missing || expected.is_none() {
            Ok(())
        } else {
            Err(Error::Conflict)
        };
    }
    if expected.is_none_or(|v| files::inspect(path).ok() != Some(files::bytes_entry(v.as_bytes())))
    {
        return Err(Error::Conflict);
    }
    Ok(())
}

fn verify_tree(
    root: &Path,
    relative: &Path,
    manifest: &Manifest,
    changed: &BTreeMap<PathBuf, Vec<u8>>,
    target: bool,
) -> Result<()> {
    let mut actual = BTreeMap::new();
    files::inventory(root, relative, &mut actual)?;
    let expected: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .filter(|(p, _)| p.starts_with(relative))
        .collect();
    if actual.len() != expected.len() {
        return Err(Error::Conflict);
    }
    for (path, entry) in actual {
        if path == Path::new(DATABASE) {
            let projection = database::projected_evidence(&root.join(&path), &manifest.paths)?;
            if Some(&projection) != manifest.database_projection.as_ref() {
                return Err(Error::Conflict);
            }
        } else if Some(&&entry) != expected.get(&path)
            && !(target
                && changed
                    .get(&path)
                    .is_some_and(|bytes| files::bytes_entry(bytes) == entry))
        {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}
