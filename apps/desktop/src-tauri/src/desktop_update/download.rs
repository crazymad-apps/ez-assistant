//! 私有临时文件与流式校验由调用任务持有；取消或错误通过 RAII 清理。
use minisign_verify::PublicKey;
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    time::Duration,
};
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

use super::{
    UpdateError,
    manifest::{self, VerifiedManifest},
};

pub(super) struct VerifiedDownload(tempfile::NamedTempFile);

impl VerifiedDownload {
    /// 安装器要求整块 bytes；只从已验证的同一句柄读入一次，不重新下载或二次全量验签。
    pub(super) fn into_bytes(mut self) -> Result<Vec<u8>, UpdateError> {
        self.0
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|_| UpdateError::Download)?;
        let mut bytes = Vec::new();
        self.0
            .as_file_mut()
            .take(manifest::FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| UpdateError::Download)?;
        if bytes.len() as u64 > manifest::FILE_LIMIT {
            return Err(UpdateError::InvalidPackage);
        }
        Ok(bytes)
    }
}

pub(super) async fn download(
    http: &reqwest::Client,
    package: &VerifiedManifest,
    key: &PublicKey,
    cancellation: &CancellationToken,
    progress: impl Fn(u64, u64),
) -> Result<VerifiedDownload, UpdateError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(UpdateError::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(30 * 60), receive(http, package, key, progress)) => result.map_err(|_| UpdateError::Download)?,
    }
}

async fn receive(
    http: &reqwest::Client,
    package: &VerifiedManifest,
    key: &PublicKey,
    progress: impl Fn(u64, u64),
) -> Result<VerifiedDownload, UpdateError> {
    let signature = manifest::signature(
        package
            .artifact
            .signature
            .as_deref()
            .ok_or(UpdateError::InvalidPackage)?,
    )?;
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|_| UpdateError::InvalidPackage)?;
    let mut response = http
        .get(package.url.clone())
        .send()
        .await
        .map_err(|_| UpdateError::Download)?;
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|size| size != package.artifact.size)
    {
        return Err(UpdateError::Download);
    }
    let temporary = tempfile::Builder::new()
        .prefix("ez-desktop-update-")
        .tempfile()
        .map_err(|_| UpdateError::Download)?;
    let mut writer = tokio::fs::File::from_std(
        temporary
            .as_file()
            .try_clone()
            .map_err(|_| UpdateError::Download)?,
    );
    let mut digest = Sha256::new();
    let mut received = 0u64;
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), response.chunk())
        .await
        .map_err(|_| UpdateError::Download)?
        .map_err(|_| UpdateError::Download)?
    {
        received += chunk.len() as u64;
        if received > package.artifact.size {
            return Err(UpdateError::InvalidPackage);
        }
        verifier.update(&chunk);
        digest.update(&chunk);
        writer
            .write_all(&chunk)
            .await
            .map_err(|_| UpdateError::Download)?;
        progress(received, package.artifact.size);
    }
    if received != package.artifact.size
        || format!("{:x}", digest.finalize()) != package.artifact.sha256
    {
        return Err(UpdateError::InvalidPackage);
    }
    verifier
        .finalize()
        .map_err(|_| UpdateError::InvalidPackage)?;
    writer.flush().await.map_err(|_| UpdateError::Download)?;
    writer.sync_all().await.map_err(|_| UpdateError::Download)?;
    drop(writer);
    Ok(VerifiedDownload(temporary))
}
