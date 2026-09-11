//! 启动前的版本链、备份与逐版本事务。调用者先持有 RuntimeInstanceGuard，业务 worker 尚未开放。
//! 初始版本与软件版本对齐；不存在旧版本账本时从 v0.25.1 开始执行。

mod admission;
mod backup;
mod compatibility;
#[cfg(test)]
mod compatibility_tests;
pub(crate) mod config_cleanup;
#[cfg(test)]
mod tests;
mod v0_25_1;
mod v0_25_2;

#[cfg(test)]
use std::fs;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{
    Connection, OpenFlags, TransactionBehavior,
    hooks::{AuthAction, AuthContext, Authorization},
};
use semver::Version;
use thiserror::Error;

use super::{BUSY_TIMEOUT, DATA_DIRECTORY, DATABASE_FILE};

const BASELINE: &str = "0.25.1";
const LEDGER: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (
    version TEXT PRIMARY KEY NOT NULL,
    applied_at_ms INTEGER NOT NULL CHECK(typeof(applied_at_ms) = 'integer' AND applied_at_ms >= 0)
)";

type Result<T> = std::result::Result<T, MigrationError>;

#[derive(Debug, Error)]
pub(crate) enum MigrationError {
    #[error("database migration manifest is invalid")]
    InvalidManifest,
    #[error("database migration ledger is invalid")]
    InvalidLedger,
    #[error("database is newer than this software; upgrade the software")]
    NewerDatabase,
    #[error("Host software is below the database minimum compatible version")]
    HostTooOld,
    #[error("database compatibility metadata is invalid")]
    InvalidCompatibility,
    #[error("database journal state cannot be inspected without possible writes")]
    UnsafeJournal,
    #[error("database changed during admission")]
    DatabaseChanged,
    #[error("database backup verification failed")]
    BackupMismatch,
    #[error("database backup could not be completed and verified")]
    BackupFailed {
        #[source]
        source: Box<MigrationError>,
    },
    #[error("database path is not a regular file")]
    InvalidPath,
    #[error("database integrity verification failed")]
    Integrity,
    #[error("database migration I/O failed")]
    Io(#[from] std::io::Error),
    #[error("database migration SQL failed")]
    Sql(#[from] rusqlite::Error),
    #[error("database migration storage preparation failed")]
    Store(#[from] assistant_runtime::StoreError),
    #[error("database migration directory preparation failed")]
    Directory(#[from] crate::config_source::RuntimeHomeError),
    #[error("database migration metadata encoding failed")]
    Json(#[from] serde_json::Error),
    #[error("database migration failed at version {version}")]
    VersionFailed {
        version: String,
        backup: Option<PathBuf>,
        #[source]
        source: Box<MigrationError>,
    },
}

/// 只允许编译进二进制的入口；apply 拥有结构分支，事务与账本始终由执行器所有。
/// validate 必须核对本版本的目标投影、保留数据与业务字段，不得在其中修补异常。
struct Migration {
    version: &'static str,
    /// None 仅用于引入兼容表之前的历史基线；后续版本显式声明且不得下降。
    min_compatible_host_version: Option<&'static str>,
    /// 无账本的既有文件必须先识别已知旧结构；新建空库不走此入口。
    validate_source: fn(&Connection) -> Result<()>,
    apply: fn(&Connection) -> Result<()>,
    validate: fn(&Connection) -> Result<()>,
}

#[derive(Debug)]
struct MigrationReport {
    applied: Vec<String>,
    backup: Option<PathBuf>,
    #[cfg(test)]
    new_database: bool,
}

/// 唯一生产 manifest；软件发版必须同时声明该版本入口，包括没有 DDL 的版本。
pub(super) fn align(
    home: &Path,
    progress: impl FnMut(super::DatabaseStartupProgress),
) -> Result<()> {
    migrate_with_progress(
        home,
        env!("CARGO_PKG_VERSION"),
        &[v0_25_1::entry(), v0_25_2::entry()],
        progress,
    )
    .map(|_| ())
}

/// 只映射脱敏诊断；底层 SQLite、路径和 SQL 不进入协议。
pub(crate) fn startup_error(
    error: &assistant_runtime::StoreError,
) -> assistant_protocol::RuntimeHostStartupError {
    use assistant_protocol::RuntimeHostStartupError::*;
    use std::error::Error;
    match error
        .source()
        .and_then(|error| error.downcast_ref::<MigrationError>())
    {
        Some(MigrationError::NewerDatabase) => DatabaseNewer,
        Some(MigrationError::HostTooOld) => DatabaseHostTooOld,
        Some(MigrationError::UnsafeJournal) => DatabaseUnsafeJournal,
        Some(MigrationError::BackupMismatch | MigrationError::BackupFailed { .. }) => BackupFailed,
        Some(
            MigrationError::VersionFailed { .. }
            | MigrationError::InvalidManifest
            | MigrationError::InvalidLedger,
        ) => MigrationFailed,
        _ => DatabaseUnavailable,
    }
}

/// 版本与 manifest 均由 Host 编译产物指定，不能接受客户端提供的目标或 SQL。
/// 测试显式传入目标；正式调用点只使用编译软件版本。
/// 每次打开只处理未完成前缀，失败保留前序已提交版本与独立备份，不自动还原或继续写入。
#[cfg(test)]
fn migrate(home: &Path, target: &str, manifest: &[Migration]) -> Result<MigrationReport> {
    migrate_with_progress(home, target, manifest, |_| {})
}

fn migrate_with_progress(
    home: &Path,
    target: &str,
    manifest: &[Migration],
    mut progress: impl FnMut(super::DatabaseStartupProgress),
) -> Result<MigrationReport> {
    use assistant_protocol::RuntimeHostStartupStage::*;
    let mut current = super::DatabaseStartupProgress {
        stage: DatabaseCheck,
        database_version: None,
        min_compatible_host_version: None,
    };
    progress(current.clone());
    let versions = validate_manifest(target, manifest)?;
    let path = home.join(DATA_DIRECTORY).join(DATABASE_FILE);
    let target_version = parse_version(target)?;
    let existing = admission::ExistingDatabase::inspect(&path)?;
    let existed = existing.is_some();
    if let Some(existing) = &existing {
        let sqlite_path = admission::sqlite_path(&path)?;
        existing.check_unchanged(&sqlite_path)?;
        let mut reader = Connection::open_with_flags(
            &sqlite_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        reader.busy_timeout(BUSY_TIMEOUT)?;
        let snapshot = reader.transaction()?;
        let minimum = compatibility::read(&snapshot)?;
        current.min_compatible_host_version = minimum.as_ref().map(ToString::to_string);
        progress(current.clone());
        compatibility::admit(minimum.as_ref(), &target_version)?;
        let completed = inspect_state(&snapshot, &versions, manifest, minimum.as_ref(), false)?;
        current.database_version = completed
            .checked_sub(1)
            .map(|index| versions[index].to_string());
        progress(current.clone());
        backup::check_integrity(&snapshot)?;
        snapshot.commit()?;
        reader.close().map_err(|(_, error)| error)?;
        existing.check_unchanged(&path)?;
    }
    if !existed {
        crate::config_source::prepare_private_directory(
            path.parent().ok_or(MigrationError::InvalidPath)?,
        )?;
        super::create_new_private_file(&path)?;
    }
    // 既有文件不使用 CREATE；打不开绝不被解释为新库。不在备份前修改 journal 模式或 schema。
    let sqlite_path = admission::sqlite_path(&path)?;
    let mut connection = Connection::open_with_flags(
        &sqlite_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    if let Some(existing) = &existing {
        existing.check_unchanged(&path)?;
        existing.check_unchanged(&sqlite_path)?;
    }
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    // 在写连接事务内再验账本和下限；只读快照不能充当后续写入授权。
    let snapshot = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let minimum = compatibility::read(&snapshot)?;
    compatibility::admit(minimum.as_ref(), &target_version)?;
    let completed = inspect_state(&snapshot, &versions, manifest, minimum.as_ref(), !existed)?;
    snapshot.commit()?;
    current.database_version = completed
        .checked_sub(1)
        .map(|index| versions[index].to_string());
    if current.database_version.is_some() {
        progress(current.clone());
    }
    let mut report = MigrationReport {
        applied: vec![],
        backup: None,
        #[cfg(test)]
        new_database: !existed,
    };
    backup::check_integrity(&connection)?;
    if completed == manifest.len() {
        // 已对齐仍核验当前投影，但不重跑 DDL 或重复生成备份。
        if let Some(last) = manifest.last() {
            validate_read_only(&connection, last.validate)?;
        }
        return Ok(report);
    }
    if existed {
        // 独立读事务冻结源数据，Backup API 和逐表精确核验都观察同一快照。
        current.stage = DatabaseBackup;
        progress(current.clone());
        let snapshot = connection.transaction()?;
        report.backup = Some(
            backup::create(&snapshot, home, &path, target).map_err(|source| {
                MigrationError::BackupFailed {
                    source: Box::new(source),
                }
            })?,
        );
        snapshot.commit()?;
    }
    for (index, migration) in manifest.iter().enumerate().skip(completed) {
        current.stage = DatabaseMigration;
        progress(current.clone());
        apply_version(&mut connection, migration, &versions, manifest, index).map_err(
            |source| MigrationError::VersionFailed {
                version: migration.version.into(),
                backup: report.backup.clone(),
                source: Box::new(source),
            },
        )?;
        report.applied.push(migration.version.into());
        current.database_version = Some(migration.version.into());
        current.min_compatible_host_version =
            migration.min_compatible_host_version.map(str::to_owned);
        progress(current.clone());
    }
    Ok(report)
}

fn validate_manifest(target: &str, manifest: &[Migration]) -> Result<Vec<Version>> {
    let target = parse_version(target).map_err(|_| MigrationError::InvalidManifest)?;
    let versions = manifest
        .iter()
        .map(|entry| parse_version(entry.version).map_err(|_| MigrationError::InvalidManifest))
        .collect::<Result<Vec<_>>>()?;
    if manifest.first().map(|entry| entry.version) != Some(BASELINE)
        || versions.last() != Some(&target)
        || versions.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(MigrationError::InvalidManifest);
    }
    let mut previous = None;
    for (index, entry) in manifest.iter().enumerate() {
        let minimum = entry
            .min_compatible_host_version
            .map(parse_version)
            .transpose()
            .map_err(|_| MigrationError::InvalidManifest)?;
        if (index > 0 && minimum.is_none())
            || minimum
                .as_ref()
                .is_some_and(|minimum| minimum > &versions[index])
            || minimum < previous
        {
            return Err(MigrationError::InvalidManifest);
        }
        previous = minimum;
    }
    Ok(versions)
}

fn parse_version(raw: &str) -> Result<Version> {
    let [major, minor, patch] =
        assistant_protocol::parse_software_version(raw).ok_or(MigrationError::InvalidLedger)?;
    Ok(Version::new(major.into(), minor.into(), patch.into()))
}

/// 调用者持有 SQLite 快照；本函数不修补结构，也不把任意无账本库解释为历史版本。
fn inspect_state(
    connection: &Connection,
    versions: &[Version],
    manifest: &[Migration],
    minimum: Option<&Version>,
    new_database: bool,
) -> Result<usize> {
    let completed = completed_prefix(connection, versions)?;
    compatibility::validate_committed(minimum, manifest, completed)?;
    if completed > 0 {
        validate_read_only(connection, manifest[completed - 1].validate)?;
    } else if !new_database {
        validate_read_only(connection, manifest[0].validate_source)?;
    }
    Ok(completed)
}

fn completed_prefix(connection: &Connection, versions: &[Version]) -> Result<usize> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let columns: Vec<(String, String, i64)> = connection
        .prepare("SELECT name, type, pk FROM pragma_table_info('schema_migrations')")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if columns
        != [
            ("version".into(), "TEXT".into(), 1),
            ("applied_at_ms".into(), "INTEGER".into(), 0),
        ]
    {
        return Err(MigrationError::InvalidLedger);
    }
    let mut applied = vec![];
    let mut statement =
        connection.prepare("SELECT version, applied_at_ms FROM schema_migrations")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let version = parse_version(&row.get::<_, String>(0)?)?;
        if row.get::<_, i64>(1)? < 0 {
            return Err(MigrationError::InvalidLedger);
        }
        if versions.last().is_some_and(|target| &version > target) {
            return Err(MigrationError::NewerDatabase);
        }
        applied.push(version);
    }
    applied.sort();
    if applied.len() > versions.len() || applied != versions[..applied.len()] {
        return Err(MigrationError::InvalidLedger);
    }
    Ok(applied.len())
}

/// 事务锁内重新检查账本，避免使用锁外快照执行已完成或跳过的迁移。
/// SQL/校验期间 authorizer 禁止事务控制、危险 PRAGMA 与直接改账本；移除钩子后执行唯一记账点。
fn apply_version(
    connection: &mut Connection,
    migration: &Migration,
    versions: &[Version],
    manifest: &[Migration],
    expected: usize,
) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if completed_prefix(&transaction, versions)? != expected {
        return Err(MigrationError::InvalidLedger);
    }
    let minimum = compatibility::read(&transaction)?;
    compatibility::admit(
        minimum.as_ref(),
        versions.last().ok_or(MigrationError::InvalidManifest)?,
    )?;
    compatibility::validate_committed(minimum.as_ref(), manifest, expected)?;
    transaction.execute_batch(LEDGER)?;
    transaction.authorizer(Some(migration_authorizer))?;
    let outcome = (migration.apply)(&transaction);
    transaction.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    // 显式回滚当前版本，避免把失败版本当作成功；后继入口不会得到执行机会。
    if let Err(error) = outcome {
        transaction.rollback()?;
        return Err(error);
    }
    validate_read_only(&transaction, migration.validate)?;
    compatibility::record(&transaction, migration.min_compatible_host_version)?;
    backup::check_integrity(&transaction)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or(MigrationError::InvalidLedger)?;
    transaction.execute(
        "INSERT INTO schema_migrations(version, applied_at_ms) VALUES (?1, ?2)",
        rusqlite::params![migration.version, timestamp],
    )?;
    if completed_prefix(&transaction, versions)? != expected + 1 {
        return Err(MigrationError::InvalidLedger);
    }
    compatibility::validate_committed(
        compatibility::read(&transaction)?.as_ref(),
        manifest,
        expected + 1,
    )?;
    transaction.commit()?;
    Ok(())
}

fn migration_authorizer(context: AuthContext<'_>) -> Authorization {
    use AuthAction::*;
    match context.action {
        Transaction { .. } | Savepoint { .. } | Attach { .. } | Detach { .. } => {
            Authorization::Deny
        }
        Pragma { pragma_name, .. }
            if !matches!(
                pragma_name.to_ascii_lowercase().as_str(),
                "table_info"
                    | "table_xinfo"
                    | "table_list"
                    | "index_list"
                    | "index_info"
                    | "foreign_key_list"
                    | "foreign_key_check"
                    | "integrity_check"
                    | "quick_check"
            ) =>
        {
            Authorization::Deny
        }
        Insert { table_name }
        | Delete { table_name }
        | Update { table_name, .. }
        | DropTable { table_name }
        | AlterTable { table_name, .. }
        | CreateTrigger { table_name, .. }
        | CreateTable { table_name }
        | CreateIndex { table_name, .. }
        | DropIndex { table_name, .. }
            if table_name.eq_ignore_ascii_case("schema_migrations")
                || table_name.eq_ignore_ascii_case("database_compatibility") =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}

/// 校验阶段只读；失败不执行任何自动修补，调用方的事务随错误回滚。
fn validate_read_only(
    connection: &Connection,
    validate: fn(&Connection) -> Result<()>,
) -> Result<()> {
    connection.pragma_update(None, "query_only", true)?;
    connection.authorizer(Some(migration_authorizer))?;
    let outcome = validate(connection);
    connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
    connection.pragma_update(None, "query_only", false)?;
    outcome
}
