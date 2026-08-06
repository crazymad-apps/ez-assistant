//! 启动时一次性解析并冻结的 Demo 配置。

use std::{fs, io, path::PathBuf};

use thiserror::Error;

use crate::cli::ServeArguments;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServeConfig {
    pub workdir: PathBuf,
    pub data_dir: PathBuf,
    pub port: u16,
    pub max_compaction_handoffs: u32,
    pub retry_transient: bool,
}

impl ServeConfig {
    pub(crate) fn resolve(arguments: ServeArguments) -> Result<Self, ConfigError> {
        let current_dir = std::env::current_dir().map_err(ConfigError::CurrentDirectory)?;
        let workdir = absolute(&current_dir, arguments.workdir);
        let workdir = fs::canonicalize(&workdir).map_err(|source| ConfigError::Workdir {
            path: workdir,
            source,
        })?;
        if !workdir.is_dir() {
            return Err(ConfigError::WorkdirNotDirectory(workdir));
        }

        let data_dir = absolute(&current_dir, arguments.data_dir);
        fs::create_dir_all(&data_dir).map_err(|source| ConfigError::DataDirectory {
            path: data_dir.clone(),
            source,
        })?;
        let data_dir =
            fs::canonicalize(&data_dir).map_err(|source| ConfigError::DataDirectory {
                path: data_dir,
                source,
            })?;

        Ok(Self {
            workdir,
            data_dir,
            port: arguments.port,
            max_compaction_handoffs: arguments.max_compaction_handoffs,
            retry_transient: arguments.retry_transient,
        })
    }
}

fn absolute(current_dir: &std::path::Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        current_dir.join(path)
    }
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("failed to read current directory")]
    CurrentDirectory(#[source] io::Error),
    #[error("workdir `{}` could not be resolved", path.display())]
    Workdir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("workdir `{}` is not a directory", .0.display())]
    WorkdirNotDirectory(PathBuf),
    #[error("data directory `{}` could not be prepared", path.display())]
    DataDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_creates_directories() {
        let root = tempfile::tempdir().expect("create temp root");
        let data_dir = root.path().join("data");
        let config = ServeConfig::resolve(ServeArguments {
            workdir: root.path().to_path_buf(),
            data_dir: data_dir.clone(),
            port: 0,
            max_compaction_handoffs: crate::cli::DEFAULT_MAX_COMPACTION_HANDOFFS,
            retry_transient: false,
        })
        .expect("resolve config");

        assert!(config.workdir.is_absolute());
        assert!(config.data_dir.is_absolute());
        assert!(data_dir.is_dir());
    }
}
