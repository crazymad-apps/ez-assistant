//! 启动前的版本链、备份与逐版本事务。调用者先持有 RuntimeInstanceGuard，业务 worker 尚未开放。
//! 初始版本与软件版本对齐；不存在旧版本账本时从 v0.25.1 开始执行。

mod backup;
pub(crate) mod config_cleanup;
#[cfg(test)]
mod tests;
mod v0_25_1;

use std::{
    fs,
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
        &[v0_25_1::entry()],
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
/// 本阶段测试显式传入目标；M3 唯一正式调用点必须传入 CARGO_PKG_VERSION。
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
    };
    progress(current.clone());
    let versions = validate_manifest(target, manifest)?;
    let path = home.join(DATA_DIRECTORY).join(DATABASE_FILE);
    let existed = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => return Err(MigrationError::InvalidPath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !existed {
        crate::config_source::prepare_private_directory(
            path.parent().ok_or(MigrationError::InvalidPath)?,
        )?;
        super::create_new_private_file(&path)?;
    }
    // 既有文件不使用 CREATE；打不开绝不被解释为新库。不在备份前修改 journal 模式或 schema。
    let mut connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    let completed = completed_prefix(&connection, &versions)?;
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
        apply_version(&mut connection, migration, &versions, index).map_err(|source| {
            MigrationError::VersionFailed {
                version: migration.version.into(),
                backup: report.backup.clone(),
                source: Box::new(source),
            }
        })?;
        report.applied.push(migration.version.into());
        current.database_version = Some(migration.version.into());
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
    Ok(versions)
}

fn parse_version(raw: &str) -> Result<Version> {
    let version = Version::parse(raw).map_err(|_| MigrationError::InvalidLedger)?;
    // build metadata 不影响 schema 顺序，不允许它制造同一语义版本的第二份记录。
    if version.to_string() != raw || !version.build.is_empty() {
        return Err(MigrationError::InvalidLedger);
    }
    Ok(version)
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
    expected: usize,
) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if completed_prefix(&transaction, versions)? != expected {
        return Err(MigrationError::InvalidLedger);
    }
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
            if table_name.eq_ignore_ascii_case("schema_migrations") =>
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
