//! 可取消、有界的普通二进制文件读取机制。

use std::io;

use agent_tools::AbsolutePath;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum BinaryReadError {
    #[error("binary read limit must be greater than zero")]
    InvalidLimit,
    #[error("binary source is not a regular file")]
    NotRegularFile,
    #[error("binary source exceeds the configured limit")]
    TooLarge,
    #[error("binary read was cancelled")]
    Cancelled,
    #[error("binary source could not be read")]
    Io(#[source] io::Error),
}

/// 从已经 resolve 的绝对路径读取当前普通文件，最多返回 `max_bytes`。
///
/// 文件类型取自打开后的句柄，避免先检查路径、再打开另一个目标。读取过程逐块观察取消；
/// 本机制不解释图片格式，也不知道 Session 目录。
pub async fn read_binary_file(
    path: &AbsolutePath,
    max_bytes: u64,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, BinaryReadError> {
    if max_bytes == 0 {
        return Err(BinaryReadError::InvalidLimit);
    }
    if cancellation.is_cancelled() {
        return Err(BinaryReadError::Cancelled);
    }
    let mut file = tokio::select! {
        result = tokio::fs::File::open(path.as_path()) => result.map_err(BinaryReadError::Io)?,
        () = cancellation.cancelled() => return Err(BinaryReadError::Cancelled),
    };
    let metadata = tokio::select! {
        result = file.metadata() => result.map_err(BinaryReadError::Io)?,
        () = cancellation.cancelled() => return Err(BinaryReadError::Cancelled),
    };
    if !metadata.is_file() {
        return Err(BinaryReadError::NotRegularFile);
    }
    if metadata.len() > max_bytes {
        return Err(BinaryReadError::TooLarge);
    }

    let initial_capacity = usize::try_from(metadata.len())
        .unwrap_or(usize::MAX)
        .min(64 * 1024);
    let mut bytes = Vec::with_capacity(initial_capacity);
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = tokio::select! {
            result = file.read(&mut chunk) => result.map_err(BinaryReadError::Io)?,
            () = cancellation.cancelled() => return Err(BinaryReadError::Cancelled),
        };
        if read == 0 {
            return Ok(bytes);
        }
        let next_len = u64::try_from(bytes.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if next_len > max_bytes {
            return Err(BinaryReadError::TooLarge);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(path: &std::path::Path) -> AbsolutePath {
        AbsolutePath::new(path.to_string_lossy().into_owned()).expect("absolute path")
    }

    #[tokio::test]
    async fn reads_regular_binary_with_an_exact_limit() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("image.bin");
        std::fs::write(&path, [0_u8, 1, 2, 255]).expect("fixture");
        assert_eq!(
            read_binary_file(&absolute(&path), 4, &CancellationToken::new())
                .await
                .expect("read"),
            vec![0, 1, 2, 255]
        );
    }

    #[tokio::test]
    async fn rejects_oversize_directory_and_pre_cancelled_reads() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("large.bin");
        std::fs::write(&path, [1_u8, 2, 3]).expect("fixture");
        assert!(matches!(
            read_binary_file(&absolute(&path), 2, &CancellationToken::new()).await,
            Err(BinaryReadError::TooLarge)
        ));
        assert!(matches!(
            read_binary_file(&absolute(root.path()), 10, &CancellationToken::new()).await,
            Err(BinaryReadError::NotRegularFile)
        ));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            read_binary_file(&absolute(&path), 10, &cancellation).await,
            Err(BinaryReadError::Cancelled)
        ));
    }
}
