//! Provider 最后成功模型目录；沿用既有数据库最低 Host 版本，不改写业务行。

use rusqlite::Connection;

use super::{Migration, MigrationError, Result, v0_25_3};

pub(super) fn entry() -> Migration {
    Migration {
        version: "0.26.0",
        min_compatible_host_version: Some("0.25.3"),
        validate_source: v0_25_3::validate,
        apply,
        validate,
    }
}

fn apply(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE providers ADD COLUMN model_catalog_refreshed_at_ms INTEGER
           CHECK(model_catalog_refreshed_at_ms IS NULL OR (typeof(model_catalog_refreshed_at_ms)='integer' AND model_catalog_refreshed_at_ms>=0));
         ALTER TABLE providers ADD COLUMN model_catalog_json TEXT
           CHECK((model_catalog_json IS NULL AND model_catalog_refreshed_at_ms IS NULL) OR
                 (model_catalog_json IS NOT NULL AND model_catalog_refreshed_at_ms IS NOT NULL AND json_valid(model_catalog_json) AND json_type(model_catalog_json)='array'));
         ALTER TABLE providers ADD COLUMN model_catalog_connection_changed INTEGER NOT NULL DEFAULT 0
           CHECK(typeof(model_catalog_connection_changed)='integer' AND model_catalog_connection_changed IN (0,1) AND
                 (model_catalog_json IS NOT NULL OR model_catalog_connection_changed=0));",
    )?;
    Ok(())
}

pub(super) fn validate(connection: &Connection) -> Result<()> {
    v0_25_3::validate(connection)?;
    connection.prepare(
        "SELECT model_catalog_json, model_catalog_refreshed_at_ms, model_catalog_connection_changed FROM providers LIMIT 0",
    )?;
    let invalid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM providers WHERE
           (model_catalog_json IS NULL)!=(model_catalog_refreshed_at_ms IS NULL) OR
           model_catalog_connection_changed NOT IN (0,1) OR
           (model_catalog_json IS NULL AND model_catalog_connection_changed!=0) OR
           (model_catalog_json IS NOT NULL AND (json_valid(model_catalog_json)=0 OR json_type(model_catalog_json)!='array')) OR
           model_catalog_refreshed_at_ms<0)",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(MigrationError::Integrity);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
