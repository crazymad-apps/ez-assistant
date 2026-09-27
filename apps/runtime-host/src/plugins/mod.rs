//! 插件包的 Host 文件边界。只检查和准备字节，安装事实由 Runtime 与 Store 持有。

mod archive;
mod capabilities;
pub(crate) mod config_files;
mod files;
mod manifest;
mod paths;

pub(crate) use archive::{ArchiveFormat, prepare_archive_with_cancel};
pub(crate) use files::HostPluginFiles;

pub(crate) use manifest::{PluginManifest, plugin_name};

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum PackageError {
    #[error("插件包不符合格式或安全规则")]
    Invalid,
    #[error("插件包超过大小或条目限制")]
    TooLarge,
    #[error("插件包校验已取消")]
    Cancelled,
    #[error("插件包文件不可用")]
    Io(#[from] std::io::Error),
}

pub(crate) type Result<T> = std::result::Result<T, PackageError>;

#[cfg(test)]
mod tests;
