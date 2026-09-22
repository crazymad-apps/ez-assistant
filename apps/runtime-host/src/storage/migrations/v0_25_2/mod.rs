//! 首次写入独立数据库下限；业务结构沿用 v0.25.1，元数据由外层控制器原子提交。

use super::{Migration, Result, v0_25_1};
use rusqlite::Connection;

pub(super) fn entry() -> Migration {
    Migration {
        version: "0.25.2",
        min_compatible_host_version: Some("0.25.2"),
        validate_source: v0_25_1::validate,
        apply: |_: &Connection| -> Result<()> { Ok(()) },
        validate: v0_25_1::validate,
    }
}
