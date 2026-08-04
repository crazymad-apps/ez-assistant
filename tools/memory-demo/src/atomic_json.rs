//! Demo 私有的原子 JSON 文件读写。

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};
use tempfile::NamedTempFile;
use thiserror::Error;

/// 原子 JSON 读写的内部错误；上层负责转换为各自领域错误。
#[derive(Debug, Error)]
pub(crate) enum AtomicJsonError {
    #[error("JSON data is invalid: {0}")]
    InvalidData(String),
    #[error("JSON file I/O failed: {0}")]
    Io(String),
}

/// 写入策略只保留一个测试故障点，避免为 Demo 建立通用存储框架。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AtomicJsonWriter {
    fail_before_write: bool,
    fail_before_persist: bool,
}

impl AtomicJsonWriter {
    #[cfg(test)]
    pub(crate) fn failing_before_write() -> Self {
        Self {
            fail_before_write: true,
            fail_before_persist: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn failing_before_persist() -> Self {
        Self {
            fail_before_write: false,
            fail_before_persist: true,
        }
    }

    /// 在目标同目录完成 write、flush、sync 和原子替换。
    pub(crate) async fn write<T>(&self, path: &Path, value: &T) -> Result<(), AtomicJsonError>
    where
        T: Serialize,
    {
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| AtomicJsonError::InvalidData(error.to_string()))?;
        bytes.push(b'\n');
        let path = path.to_owned();
        let fail_before_write = self.fail_before_write;
        let fail_before_persist = self.fail_before_persist;
        tokio::task::spawn_blocking(move || {
            write_bytes(path, bytes, fail_before_write, fail_before_persist)
        })
        .await
        .map_err(|error| AtomicJsonError::Io(format!("write task failed: {error}")))?
    }
}

/// 读取完整 JSON；文件不存在由调用方作为空状态或明确错误处理。
pub(crate) async fn read<T>(path: &Path) -> Result<Option<T>, AtomicJsonError>
where
    T: DeserializeOwned + Send + 'static,
{
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AtomicJsonError::Io(error.to_string())),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| AtomicJsonError::InvalidData(error.to_string()))
    })
    .await
    .map_err(|error| AtomicJsonError::Io(format!("read task failed: {error}")))?
}

fn write_bytes(
    path: PathBuf,
    bytes: Vec<u8>,
    fail_before_write: bool,
    fail_before_persist: bool,
) -> Result<(), AtomicJsonError> {
    let parent = path.parent().ok_or_else(|| {
        AtomicJsonError::Io("target JSON file does not have a parent directory".to_owned())
    })?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(io_error)?;
    if fail_before_write {
        return Err(AtomicJsonError::Io(
            "injected failure before temporary-file write".to_owned(),
        ));
    }
    temporary.write_all(&bytes).map_err(io_error)?;
    temporary.flush().map_err(io_error)?;
    temporary.as_file().sync_all().map_err(io_error)?;
    if fail_before_persist {
        return Err(AtomicJsonError::Io(
            "injected failure before atomic persist".to_owned(),
        ));
    }
    temporary
        .persist(&path)
        .map_err(|error| io_error(error.error))?;
    Ok(())
}

fn io_error(error: io::Error) -> AtomicJsonError {
    AtomicJsonError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Record {
        value: u32,
    }

    #[tokio::test]
    async fn write_replaces_atomically_and_failure_preserves_old_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        let writer = AtomicJsonWriter::default();
        writer
            .write(&path, &Record { value: 1 })
            .await
            .expect("write first record");

        let failing_write = AtomicJsonWriter::failing_before_write();
        assert!(
            failing_write
                .write(&path, &Record { value: 2 })
                .await
                .is_err()
        );
        let failing = AtomicJsonWriter::failing_before_persist();
        assert!(failing.write(&path, &Record { value: 3 }).await.is_err());
        assert_eq!(
            read::<Record>(&path).await.expect("read record"),
            Some(Record { value: 1 })
        );
    }
}
