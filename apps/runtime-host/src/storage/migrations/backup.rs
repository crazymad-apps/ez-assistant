//! 独立 SQLite、会话历史与配置的配套备份；不扫描项目目录，核验完成后才发布清单。

#[cfg(test)]
mod managed_tests;

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
    types::ValueRef,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MigrationError, Result};
use crate::host_layout::files::{self, Entry};

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct TableEvidence {
    rows: u64,
    /// 对全部原始列进行有序、带 SQLite 类型和长度的摘要，含主键、模型与文件引用字段。
    sha256: String,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct DatabaseEvidence {
    pub(super) schema_sha256: String,
    pub(super) tables: BTreeMap<String, TableEvidence>,
}

/// 调用者持有源库读快照和进程独占锁。返回目录前重新只读连接备份并精确核验所有普通表。
/// 出错保留独立目录供诊断，不自动覆盖/删除旧备份；清单最后写入，存在半成品不代表验证成功。
pub(super) fn create_with_progress(
    source: &Connection,
    home: &Path,
    source_path: &Path,
    target: &str,
    mut progress: impl FnMut(
        assistant_protocol::RuntimeHostStartupStage,
        Option<assistant_protocol::RuntimeHostStartupProgress>,
    ),
) -> Result<PathBuf> {
    use assistant_protocol::{RuntimeHostStartupProgress, RuntimeHostStartupStage::*};
    progress(DatabaseBackup, None);
    let expected = evidence(source)?;
    let root = home.join("backups/database");
    crate::config_source::prepare_private_directory(&root)?;
    let directory = tempfile::Builder::new()
        .prefix("upgrade-")
        .tempdir_in(&root)?
        .keep();
    let database = directory.join("runtime.sqlite3");
    super::super::create_new_private_file(&database)?;
    let mut destination =
        Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    {
        let backup = Backup::new(source, &mut destination)?;
        loop {
            let step = backup.step(256)?;
            let pages = backup.progress();
            progress(
                DatabaseBackup,
                Some(RuntimeHostStartupProgress {
                    completed: u64::try_from(pages.pagecount - pages.remaining)
                        .map_err(|_| MigrationError::Integrity)?,
                    total: u64::try_from(pages.pagecount).map_err(|_| MigrationError::Integrity)?,
                }),
            );
            match step {
                StepResult::Done => break,
                StepResult::More => {}
                // 启动没有业务 writer；不在异常锁竞争时无限重试或写入源库补救。
                _ => return Err(MigrationError::BackupMismatch),
            }
        }
    }
    destination.close().map_err(|(_, error)| error)?;
    // Windows 的 FlushFileBuffers 需要写权限句柄，只读打开后 sync 会拒绝访问。
    crate::platform::sync_file_at(&database)?;
    verify(&database, &expected)?;

    // 白名单只枚举受管主会话／child 历史，不进入 private、附件、依赖或外部工作区。
    progress(HistoryBackup, None);
    let managed_files = managed_inventory(home)?;
    let total = managed_files
        .values()
        .filter(|entry| !matches!(entry, Entry::Directory))
        .count() as u64;
    let mut completed = 0;
    progress(
        HistoryBackup,
        Some(RuntimeHostStartupProgress { completed, total }),
    );
    for (relative, entry) in &managed_files {
        files::copy_entry(&home.join(relative), &directory.join(relative), entry)
            .map_err(managed_files_error)?;
        if !matches!(entry, Entry::Directory) {
            completed += 1;
            progress(
                HistoryBackup,
                Some(RuntimeHostStartupProgress { completed, total }),
            );
        }
    }
    progress(
        BackupVerification,
        Some(RuntimeHostStartupProgress {
            completed: 0,
            total: 2,
        }),
    );
    if managed_inventory(home)? != managed_files {
        return Err(MigrationError::BackupMismatch);
    }
    progress(
        BackupVerification,
        Some(RuntimeHostStartupProgress {
            completed: 1,
            total: 2,
        }),
    );
    if managed_inventory(&directory)? != managed_files {
        return Err(MigrationError::BackupMismatch);
    }
    progress(
        BackupVerification,
        Some(RuntimeHostStartupProgress {
            completed: 2,
            total: 2,
        }),
    );
    for (relative, entry) in managed_files.iter().rev() {
        if matches!(entry, Entry::Directory) {
            super::super::sync_directory(&directory.join(relative))?;
        }
    }

    let config_path = home.join("config.toml");
    let config_hash = match fs::symlink_metadata(&config_path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= 1024 * 1024 => {
            let contents = fs::read(&config_path)?;
            let output = directory.join("config.toml");
            let mut file = super::super::create_new_private_file(&output)?;
            file.write_all(&contents)?;
            file.sync_all()?;
            let digest = Sha256::digest(&contents);
            if digest != Sha256::digest(fs::read(&output)?) {
                return Err(MigrationError::BackupMismatch);
            }
            Some(format!("{digest:x}"))
        }
        Ok(_) => return Err(MigrationError::InvalidPath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let manifest = serde_json::json!({
        "source_database": source_path,
        "target_version": target,
        "database": expected,
        "config_sha256": config_hash,
        "external_files_included": true,
        "file_scope": "session_history_only",
        "managed_files": managed_files,
    });
    let contents = serde_json::to_vec_pretty(&manifest)?;
    let output = directory.join("manifest.json");
    let mut file = super::super::create_new_private_file(&output)?;
    file.write_all(&contents)?;
    file.sync_all()?;
    if fs::read(&output)? != contents {
        return Err(MigrationError::BackupMismatch);
    }
    super::super::sync_directory(&directory)?;
    super::super::sync_directory(&root)?;
    Ok(directory)
}

/// 只释放调用方本轮创建的备份目录，不枚举或清理其他备份；不能跟随顶层链接。
pub(super) fn remove_after_upgrade(home: &Path, directory: &Path) -> Result<()> {
    let root = home.join("backups/database");
    if directory.parent() != Some(root.as_path()) {
        return Err(MigrationError::InvalidPath);
    }
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || crate::platform::is_reparse_or_link(&metadata) {
        return Err(MigrationError::InvalidPath);
    }
    fs::remove_dir_all(directory)?;
    super::super::sync_directory(&root)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn create(
    source: &Connection,
    home: &Path,
    source_path: &Path,
    target: &str,
) -> Result<PathBuf> {
    create_with_progress(source, home, source_path, target, |_, _| {})
}

/// 仅备份受管历史路径的所有代次；不递归扫描会话目录，项目里的同名文件也不纳入。
/// SQLite 及日志由 Backup API 负责，附件／工具图片等未被迁移改写的资源仍留在原位置。
fn managed_inventory(home: &Path) -> Result<BTreeMap<PathBuf, Entry>> {
    let mut entries = BTreeMap::new();
    let data = Path::new(super::super::DATA_DIRECTORY);
    if !inventory_directory(home, data, &mut entries)? {
        return Ok(entries);
    }
    let staging = data.join("staging");
    let has_staging = inventory_directory(home, &staging, &mut entries)?;
    let roots = [
        data.join(super::super::SESSIONS_DIRECTORY),
        staging.join("deletions"),
    ];
    // 尚未提交的删除会先把整个 Session 移到 staging/deletions；升级恢复会改动其中历史。
    // 只保全相同层级的历史文件，仍不进入该 Session 的 private／资源目录。
    for (index, sessions) in roots.iter().enumerate() {
        if index == 1 && !has_staging {
            continue;
        }
        if !inventory_directory(home, sessions, &mut entries)? {
            continue;
        }
        for session in fs::read_dir(home.join(sessions))? {
            let session = session?;
            if !session.file_type()?.is_dir() {
                continue;
            }
            let relative = sessions.join(session.file_name());
            inventory_directory(home, &relative, &mut entries)?;
            inventory_history_files(home, &relative, "conversation.", &mut entries)?;
            let children = relative.join("child-tasks");
            if !inventory_directory(home, &children, &mut entries)? {
                continue;
            }
            for child in fs::read_dir(home.join(&children))? {
                let child = child?;
                if !child.file_type()?.is_dir() {
                    continue;
                }
                let relative = children.join(child.file_name());
                inventory_directory(home, &relative, &mut entries)?;
                inventory_history_files(home, &relative, "body-", &mut entries)?;
            }
        }
    }
    Ok(entries)
}

fn inventory_directory(
    home: &Path,
    relative: &Path,
    entries: &mut BTreeMap<PathBuf, Entry>,
) -> Result<bool> {
    if !files::exists(&home.join(relative)).map_err(managed_files_error)? {
        return Ok(false);
    }
    if files::inspect(&home.join(relative)).map_err(managed_files_error)? != Entry::Directory {
        return Err(MigrationError::InvalidPath);
    }
    entries.insert(relative.to_owned(), Entry::Directory);
    Ok(true)
}

fn inventory_history_files(
    home: &Path,
    relative: &Path,
    prefix: &str,
    entries: &mut BTreeMap<PathBuf, Entry>,
) -> Result<()> {
    for file in fs::read_dir(home.join(relative))? {
        let file = file?;
        let name = file.file_name();
        let Some(generation) = name
            .to_str()
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|name| name.strip_suffix(".jsonl"))
        else {
            continue;
        };
        if !generation.parse::<u64>().is_ok_and(|value| value > 0) {
            continue;
        }
        let path = relative.join(name);
        let entry = files::inspect(&home.join(&path)).map_err(managed_files_error)?;
        if matches!(entry, Entry::Directory) {
            return Err(MigrationError::InvalidPath);
        }
        entries.insert(path, entry);
    }
    Ok(())
}

fn managed_files_error(source: crate::host_layout::Error) -> MigrationError {
    MigrationError::Store(super::super::internal_error(
        "managed data backup could not be verified",
        source,
    ))
}

fn verify(path: &Path, expected: &DatabaseEvidence) -> Result<()> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    check_integrity(&connection)?;
    if &evidence(&connection)? != expected {
        return Err(MigrationError::BackupMismatch);
    }
    Ok(())
}

pub(super) fn check_integrity(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if values != ["ok"]
        || connection
            .prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some()
    {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}

pub(super) fn evidence(connection: &Connection) -> Result<DatabaseEvidence> {
    let schema = fingerprint(
        connection,
        "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name, tbl_name, sql",
    )?;
    // table_list 排除 virtual 和 shadow；普通内容表仍逐表核验，schema 摘要包含 FTS 定义。
    let names = connection.prepare("SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'table' AND substr(name, 1, 7) != 'sqlite_' ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tables = BTreeMap::new();
    for name in names {
        let identifier = format!("\"{}\"", name.replace('"', "\"\""));
        let columns = connection
            .prepare(&format!("SELECT * FROM {identifier} LIMIT 0"))?
            .column_count();
        let order = (1..=columns)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {identifier}"), [], |row| {
                row.get(0)
            })?;
        let actual = fingerprint(
            connection,
            &format!("SELECT * FROM {identifier} ORDER BY {order}"),
        )?;
        if i64::try_from(actual.rows).ok() != Some(count) {
            return Err(MigrationError::BackupMismatch);
        }
        tables.insert(name, actual);
    }
    Ok(DatabaseEvidence {
        schema_sha256: schema.sha256,
        tables,
    })
}

fn fingerprint(connection: &Connection, query: &str) -> Result<TableEvidence> {
    let mut statement = connection.prepare(query)?;
    let columns = statement.column_count();
    let mut cursor = statement.query([])?;
    let mut digest = Sha256::new();
    let mut rows = 0;
    while let Some(row) = cursor.next()? {
        digest.update([255]);
        for index in 0..columns {
            match row.get_ref(index)? {
                ValueRef::Null => digest.update([0]),
                ValueRef::Integer(value) => {
                    digest.update([1]);
                    digest.update(value.to_be_bytes());
                }
                ValueRef::Real(value) => {
                    digest.update([2]);
                    digest.update(value.to_bits().to_be_bytes());
                }
                ValueRef::Text(value) | ValueRef::Blob(value) => {
                    digest.update([if matches!(row.get_ref(index)?, ValueRef::Text(_)) {
                        3
                    } else {
                        4
                    }]);
                    digest.update((value.len() as u64).to_be_bytes());
                    digest.update(value);
                }
            }
        }
        rows += 1;
    }
    Ok(TableEvidence {
        rows,
        sha256: format!("{:x}", digest.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_detects_same_count_changed_fields_and_missing_files() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("copy.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE samples(id TEXT PRIMARY KEY, reference BLOB); INSERT INTO samples VALUES('s', X'1234')").unwrap();
        connection.execute_batch("CREATE TABLE sqlitex_extra(value TEXT); INSERT INTO sqlitex_extra VALUES('included')").unwrap();
        let original = evidence(&connection).unwrap();
        assert_eq!(original.tables["sqlitex_extra"].rows, 1);
        verify(&path, &original).unwrap();
        connection
            .execute("UPDATE samples SET reference=X'5678'", [])
            .unwrap();
        assert!(matches!(
            verify(&path, &original),
            Err(MigrationError::BackupMismatch)
        ));
        assert!(verify(&home.path().join("missing.sqlite3"), &original).is_err());
        assert!(!home.path().join("missing.sqlite3").exists());
    }
}
