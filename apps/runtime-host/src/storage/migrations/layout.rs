//! Host 布局升级的数据库边界。所有文件锁由外层持有；不创建业务 Store 或恢复会话。

use super::{backup::DatabaseEvidence, *};
use crate::host_layout::paths::Relocation;
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use std::path::Path;

fn manifest() -> [Migration; 5] {
    [
        v0_25_1::entry(),
        v0_25_2::entry(),
        v0_25_3::entry(),
        v0_26_0::entry(),
        v0_27_0::entry(),
    ]
}

/// 只读核对真实源库；无文件不是创建空库的许可，由布局层检查配套数据是否存在。
pub(crate) fn admit(path: &Path) -> Result<Option<Connection>> {
    let Some(existing) = admission::ExistingDatabase::inspect(path)? else {
        return Ok(None);
    };
    let sqlite_path = admission::sqlite_path(path)?;
    existing.check_unchanged(&sqlite_path)?;
    let connection = Connection::open_with_flags(
        &sqlite_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let target = parse_version(env!("CARGO_PKG_VERSION"))?;
    let entries = manifest();
    let versions = validate_manifest(env!("CARGO_PKG_VERSION"), &entries)?;
    let minimum = compatibility::read(&connection)?;
    compatibility::admit(minimum.as_ref(), &target)?;
    inspect_state(&connection, &versions, &entries, minimum.as_ref(), false)?;
    backup::check_integrity(&connection)?;
    existing.check_unchanged(path)?;
    Ok(Some(connection))
}

/// Online Backup 写入独立副本并回开核验全表数量、内容与 Schema；失败不改源库。
pub(crate) fn copy(source: &Path, destination: &Path) -> Result<()> {
    let mut reader = admit(source)?.ok_or(MigrationError::InvalidPath)?;
    let snapshot = reader.transaction()?;
    let expected = backup::evidence(&snapshot)?;
    super::super::create_new_private_file(destination)?;
    let mut writer = Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    copy_connection(&snapshot, &mut writer)?;
    writer.close().map_err(|(_, e)| e)?;
    crate::platform::sync_file_at(destination)?;
    let verified = Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    backup::check_integrity(&verified)?;
    if backup::evidence(&verified)? != expected {
        return Err(MigrationError::BackupMismatch);
    }
    Ok(())
}

fn copy_connection(source: &Connection, destination: &mut Connection) -> Result<()> {
    let backup = Backup::new(source, destination)?;
    loop {
        match backup.step(256)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            _ => return Err(MigrationError::BackupMismatch),
        }
    }
}

/// 在独立内存副本上计算确定性的目标投影，供重入时证明已提交库来自该备份。
/// 仅排除账本的执行时间，账本版本和兼容下限仍由 admit 严格核对。
pub(crate) fn projected_evidence(source: &Path, paths: &Relocation) -> Result<serde_json::Value> {
    let source = admit(source)?.ok_or(MigrationError::InvalidPath)?;
    let mut expected = Connection::open_in_memory()?;
    copy_connection(&source, &mut expected)?;
    expected.pragma_update(None, "foreign_keys", true)?;
    let entries = manifest();
    let versions = validate_manifest(env!("CARGO_PKG_VERSION"), &entries)?;
    let completed = completed_prefix(&expected, &versions)?;
    for (index, migration) in entries.iter().enumerate().skip(completed) {
        apply_version(
            &mut expected,
            migration,
            &versions,
            &entries,
            index,
            Some(paths),
        )?;
    }
    evidence(&expected)
}

fn evidence(connection: &Connection) -> Result<serde_json::Value> {
    let DatabaseEvidence {
        schema_sha256,
        mut tables,
    } = backup::evidence(connection)?;
    tables.remove("schema_migrations");
    Ok(serde_json::json!({"schema_sha256": schema_sha256, "tables": tables}))
}

pub(crate) fn verify(path: &Path, expected: &serde_json::Value) -> Result<()> {
    let connection = admit(path)?.ok_or(MigrationError::InvalidPath)?;
    if evidence(&connection)? != *expected {
        return Err(MigrationError::BackupMismatch);
    }
    Ok(())
}

/// 原文件已原子迁到用户域、整套备份已验证，之后才允许这条具体版本链修改目标库。
pub(crate) fn upgrade(home: &Path, paths: &Relocation) -> Result<()> {
    migrate_layout_with_progress(
        home,
        env!("CARGO_PKG_VERSION"),
        &manifest(),
        |_| {},
        Some(paths),
    )
    .map(|_| ())
}

#[cfg(test)]
pub(crate) fn legacy_fixture(home: &Path) {
    migrate(home, "0.26.0", &manifest()[..4]).unwrap();
    let connection = Connection::open(home.join("data/runtime.sqlite3")).unwrap();
    let managed = home.join("data/workspaces/w/agent");
    let external = home.join("external");
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    connection.execute("INSERT INTO workspaces(workspace_id,label,user_directory,agent_directory,lifecycle,created_at_ms,updated_at_ms,additional_directories_json) VALUES('w','kept label',?1,?2,'active',1,1,?3)",
        rusqlite::params![external.to_str().unwrap(), managed.to_str().unwrap(), serde_json::to_string(&[managed.to_str().unwrap(), external.to_str().unwrap()]).unwrap()]).unwrap();
}
