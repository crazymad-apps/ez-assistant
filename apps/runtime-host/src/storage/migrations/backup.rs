//! 独立 SQLite 快照及可重新读取的核验清单；不复制附件/Conversation 文件，不执行恢复。

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
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{MigrationError, Result};

#[derive(Debug, Eq, PartialEq, Serialize)]
struct TableEvidence {
    rows: u64,
    /// 对全部原始列进行有序、带 SQLite 类型和长度的摘要，含主键、模型与文件引用字段。
    sha256: String,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct DatabaseEvidence {
    schema_sha256: String,
    tables: BTreeMap<String, TableEvidence>,
}

/// 调用者持有源库读快照和进程独占锁。返回目录前重新只读连接备份并精确核验所有普通表。
/// 出错保留独立目录供诊断，不自动覆盖/删除旧备份；清单最后写入，存在半成品不代表验证成功。
pub(super) fn create(
    source: &Connection,
    home: &Path,
    source_path: &Path,
    target: &str,
) -> Result<PathBuf> {
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
            match backup.step(256)? {
                StepResult::Done => break,
                StepResult::More => {}
                // 启动没有业务 writer；不在异常锁竞争时无限重试或写入源库补救。
                _ => return Err(MigrationError::BackupMismatch),
            }
        }
    }
    destination.close().map_err(|(_, error)| error)?;
    fs::File::open(&database)?.sync_all()?;
    verify(&database, &expected)?;

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
        "external_files_included": false,
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

fn evidence(connection: &Connection) -> Result<DatabaseEvidence> {
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
