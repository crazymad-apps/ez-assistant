//! Runtime Host 拥有的本地持久化 Adapter。
//!
//! `LocalRuntimeStore` 只负责把 Runtime 的业务存储命令送入一个有界队列。专用阻塞线程
//! 从初始化到关闭独占 SQLite connection 和 Conversation 文件 I/O，Tokio worker 不直接
//! 执行阻塞数据库或文件操作。
//!
//! `recovery` 只处理正文文件 staged append/generation；`append_effect` 描述文件提交后的业务效果；
//! `input_state`、`run_state` 和 `run_projection` 分别负责输入、Run 状态转换与数据库投影。

mod append_effect;
mod attachment;
mod attachment_io;
mod child_task;
mod context_replacement;
mod conversation;
mod device;
mod engine;
mod filesystem;
mod goal;
mod input_state;
mod materialization;
mod memory;
mod mode;
mod permission;
mod recall_index;
mod recovery;
mod run_projection;
mod run_state;
mod schema;
mod session_history;
mod session_management;
mod session_resources;
mod session_transfer;
mod skill;
mod tool_exchange;
mod tool_images;
mod usage;
mod work_plan;
mod worker;
mod workspace;

#[cfg(test)]
mod tests;

use std::{collections::BTreeSet, path::Path, time::Duration};

use assistant_runtime::StoreError;

pub(crate) use worker::LocalRuntimeStore;

use engine::StorageEngine;
use filesystem::{
    body_path, child_body_path, child_task_directory, child_tasks_directory, conflict,
    create_new_private_file, database_write_error, internal_error, invalid_data,
    invalid_data_with_source, non_negative_u64, positive_u64, sync_directory, to_i64,
};

const DATA_DIRECTORY: &str = "data";
const SESSIONS_DIRECTORY: &str = "sessions";
const WORKSPACES_DIRECTORY: &str = "workspaces";
const BLOBS_DIRECTORY: &str = "blobs";
const STAGING_DIRECTORY: &str = "staging/uploads";
const DELETION_STAGING_DIRECTORY: &str = "staging/deletions";
const DATABASE_FILE: &str = "runtime.sqlite3";
const PRIVATE_FILE_MODE: u32 = 0o600;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type StorageResult<T> = Result<T, StoreError>;

/// 解码持久化的有序附加目录，并在 Store 边界拒绝损坏或越界 JSON。
fn decode_additional_directories(
    value: &str,
    invalid_message: &'static str,
) -> StorageResult<Vec<String>> {
    let directories = serde_json::from_str::<Vec<String>>(value)
        .map_err(|source| invalid_data_with_source(invalid_message, source))?;
    if directories.len() > 15 {
        return Err(invalid_data(invalid_message));
    }
    let mut unique = BTreeSet::new();
    for directory in &directories {
        if !Path::new(directory).is_absolute() || !unique.insert(directory.as_str()) {
            return Err(invalid_data(invalid_message));
        }
    }
    Ok(directories)
}

#[cfg(test)]
mod directory_decode_tests {
    use super::*;

    #[test]
    fn additional_directory_json_round_trips_in_order() {
        assert_eq!(
            decode_additional_directories(
                r#"["/workspace/docs","/workspace/assets"]"#,
                "invalid directories",
            )
            .expect("valid directory list"),
            ["/workspace/docs", "/workspace/assets"]
        );
    }

    #[test]
    fn additional_directory_json_fails_closed_when_damaged_or_invalid() {
        for value in [
            "not-json",
            r#"{"path":"/workspace/docs"}"#,
            r#"["relative/path"]"#,
            r#"["/workspace/docs","/workspace/docs"]"#,
        ] {
            assert!(
                decode_additional_directories(value, "invalid directories").is_err(),
                "value should be rejected: {value}"
            );
        }

        let too_many = serde_json::to_string(
            &(0..16)
                .map(|index| format!("/workspace/{index}"))
                .collect::<Vec<_>>(),
        )
        .expect("encode fixture");
        assert!(decode_additional_directories(&too_many, "invalid directories").is_err());
    }
}
