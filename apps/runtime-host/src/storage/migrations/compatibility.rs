//! 数据库独立软件下限。仅迁移控制器可在版本事务中建立或提升，不随 Host 启动覆写。

use super::{Migration, MigrationError, Result, parse_version};
use rusqlite::{Connection, OptionalExtension};
use semver::Version;

const TABLE: &str = "CREATE TABLE database_compatibility (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    min_compatible_host_version TEXT NOT NULL
)";

/// 先读独立下限，再判断账本；旧 Host 对高下限库优先给出明确拒绝原因。
pub(super) fn read(connection: &Connection) -> Result<Option<Version>> {
    let definition: Option<(String, Option<String>)> = connection.query_row(
        "SELECT type, sql FROM sqlite_schema WHERE name = 'database_compatibility' COLLATE NOCASE",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((kind, Some(sql))) = definition else {
        return if definition.is_none() {
            Ok(None)
        } else {
            Err(MigrationError::InvalidCompatibility)
        };
    };
    let normalize = |text: &str| {
        text.chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    if kind != "table" || normalize(&sql) != normalize(TABLE) {
        return Err(MigrationError::InvalidCompatibility);
    }
    let triggers: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'trigger' AND tbl_name = 'database_compatibility' COLLATE NOCASE)",
        [], |row| row.get(0),
    )?;
    if triggers {
        return Err(MigrationError::InvalidCompatibility);
    }
    let mut statement =
        connection.prepare("SELECT id, min_compatible_host_version FROM database_compatibility")?;
    let mut rows = statement.query([])?;
    let row = rows.next()?.ok_or(MigrationError::InvalidCompatibility)?;
    if row.get::<_, i64>(0)? != 1 {
        return Err(MigrationError::InvalidCompatibility);
    }
    let value: String = row.get(1)?;
    let minimum = parse_version(&value).map_err(|_| MigrationError::InvalidCompatibility)?;
    if rows.next()?.is_some() {
        return Err(MigrationError::InvalidCompatibility);
    }
    Ok(Some(minimum))
}

pub(super) fn admit(minimum: Option<&Version>, target: &Version) -> Result<()> {
    if minimum.is_some_and(|minimum| minimum > target) {
        return Err(MigrationError::HostTooOld);
    }
    Ok(())
}

/// 下限必须与已提交版本的声明一致。缺失或降低不能被伪装成一次新的旧库升级。
pub(super) fn validate_committed(
    minimum: Option<&Version>,
    manifest: &[Migration],
    completed: usize,
) -> Result<()> {
    let expected = completed
        .checked_sub(1)
        .and_then(|index| manifest[index].min_compatible_host_version)
        .map(parse_version)
        .transpose()?;
    if minimum != expected.as_ref() {
        return Err(MigrationError::InvalidCompatibility);
    }
    Ok(())
}

/// 调用者已完成迁移与只读业务校验，且仍在同一版本事务内；失败与本次业务及账本一起回滚。
/// 仅历史基线允许无声明；相同下限不写入，较低声明拒绝，不提供外部修改入口。
pub(super) fn record(connection: &Connection, declared: Option<&str>) -> Result<()> {
    let current = read(connection)?;
    let Some(declared) = declared else {
        return if current.is_none() {
            Ok(())
        } else {
            Err(MigrationError::InvalidCompatibility)
        };
    };
    let desired = parse_version(declared)?;
    match current {
        Some(current) if current > desired => return Err(MigrationError::InvalidCompatibility),
        Some(current) if current == desired => return Ok(()),
        Some(_) => {
            connection.execute(
                "UPDATE database_compatibility SET min_compatible_host_version = ?1 WHERE id = 1",
                [declared],
            )?;
        }
        None => {
            connection.execute_batch(TABLE)?;
            connection.execute(
                "INSERT INTO database_compatibility VALUES (1, ?1)",
                [declared],
            )?;
        }
    }
    if read(connection)?.as_ref() != Some(&desired) {
        return Err(MigrationError::InvalidCompatibility);
    }
    Ok(())
}
