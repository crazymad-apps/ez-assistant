//! 不访问文件系统的绝对逻辑路径类型与 Session 工作目录解析器。
//!
//! 本模块只做词法归一化：消解 `.`、`..` 并把相对输入拼到显式工作目录。它不会调用
//! `exists`、`metadata`、`canonicalize` 或解析符号链接，因此结果表示授权与审计使用的
//! 逻辑路径，不表示文件系统中的物理真实路径。

use std::{
    fmt,
    path::{Path, PathBuf},
};

use path_absolutize::Absolutize;
use serde::{Serialize, Serializer};
use thiserror::Error;

/// 已完成词法归一化、可无损表示为 UTF-8 的绝对逻辑路径。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AbsolutePath(PathBuf);

impl AbsolutePath {
    /// 校验并词法归一化一个绝对路径；不访问文件系统。
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, PathResolutionError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(PathResolutionError::NotAbsolute { path });
        }
        let normalized = path.as_path().absolutize_from(Path::new("")).into_owned();
        if normalized.to_str().is_none() {
            return Err(PathResolutionError::NonUtf8);
        }
        Ok(Self(normalized))
    }

    /// 返回平台原生路径引用。
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// 返回无损 UTF-8 表示；构造阶段已经保证一定存在。
    pub fn as_str(&self) -> &str {
        self.0
            .to_str()
            .expect("AbsolutePath constructor guarantees UTF-8")
    }

    /// 消费包装并取回平台原生路径。
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for AbsolutePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for AbsolutePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for AbsolutePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// 词法路径解析失败；不包含文件是否存在等 I/O 结论。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PathResolutionError {
    /// 输入路径为空字符串。
    #[error("path must not be empty")]
    Empty,
    /// 需要绝对路径的位置收到了相对路径。
    #[error("path is not absolute: {path:?}")]
    NotAbsolute {
        /// 原始平台路径。
        path: PathBuf,
    },
    /// 平台路径无法无损表示为 UTF-8。
    #[error("path is not valid UTF-8")]
    NonUtf8,
}

/// 基于显式 Session 工作目录解析模型路径的纯词法解析器。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPathResolver {
    session_workdir: AbsolutePath,
}

impl SessionPathResolver {
    /// 使用已经校验的绝对 Session 工作目录创建解析器。
    pub fn new(session_workdir: AbsolutePath) -> Self {
        Self { session_workdir }
    }

    /// 当前解析器冻结的 Session 工作目录。
    pub fn session_workdir(&self) -> &AbsolutePath {
        &self.session_workdir
    }

    /// 将绝对或相对 UTF-8 输入词法归一化成绝对逻辑路径。
    pub fn resolve(&self, input: &str) -> Result<AbsolutePath, PathResolutionError> {
        if input.is_empty() {
            return Err(PathResolutionError::Empty);
        }
        #[cfg(windows)]
        if !Path::new(input).is_absolute()
            && matches!(
                Path::new(input).components().next(),
                Some(std::path::Component::Prefix(_))
            )
        {
            // `C:foo` 依赖该盘符的进程级当前目录，不等同于 `C:\\foo`。
            // Session 解析不能引入这份隐藏状态，因此明确拒绝。
            return Err(PathResolutionError::NotAbsolute {
                path: PathBuf::from(input),
            });
        }
        let normalized = Path::new(input)
            .absolutize_from(self.session_workdir.as_path())
            .into_owned();
        AbsolutePath::new(normalized)
    }
}

impl TryFrom<PathBuf> for AbsolutePath {
    type Error = PathResolutionError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn root() -> AbsolutePath {
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\workspace\project");
        #[cfg(not(windows))]
        let path = PathBuf::from("/workspace/project");
        AbsolutePath::new(path).expect("valid test root")
    }

    #[test]
    fn resolves_relative_absolute_and_dot_segments_without_io() {
        let resolver = SessionPathResolver::new(root());
        let relative = resolver
            .resolve("src/./nested/../lib.rs")
            .expect("relative path resolves");
        assert_eq!(
            relative,
            root()
                .as_path()
                .join("src/lib.rs")
                .try_into()
                .expect("absolute")
        );

        let missing = resolver
            .resolve("does/not/exist.txt")
            .expect("nonexistent path still resolves lexically");
        assert!(missing.as_path().ends_with("does/not/exist.txt"));

        let absolute = resolver
            .resolve(root().as_str())
            .expect("absolute resolves");
        assert_eq!(absolute, root());

        let absolute_with_dots = resolver
            .resolve(&root().as_path().join("src/../Cargo.toml").to_string_lossy())
            .expect("absolute path with dot segments resolves");
        assert_eq!(
            absolute_with_dots.as_path(),
            root().as_path().join("Cargo.toml")
        );
    }

    #[test]
    fn normalization_is_lexical_and_does_not_confine_to_workdir() {
        let resolver = SessionPathResolver::new(root());
        let outside = resolver
            .resolve("../../outside.txt")
            .expect("parent segments resolve lexically");
        assert_eq!(
            outside,
            AbsolutePath::new(root().as_path().join("../../outside.txt"))
                .expect("normalize expected path")
        );

        let repeated = resolver
            .resolve("src//nested///")
            .expect("repeated and trailing separators resolve");
        assert_eq!(repeated.as_path(), root().as_path().join("src/nested"));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_drive_relative_input() {
        let resolver = SessionPathResolver::new(root());
        assert!(matches!(
            resolver.resolve(r"C:relative.txt"),
            Err(PathResolutionError::NotAbsolute { .. })
        ));
    }

    #[test]
    fn different_workdirs_produce_different_absolute_paths() {
        let first = SessionPathResolver::new(root())
            .resolve("file.txt")
            .expect("first resolves");
        let other_root = AbsolutePath::new(root().as_path().join("other")).expect("other root");
        let second = SessionPathResolver::new(other_root)
            .resolve("file.txt")
            .expect("second resolves");
        assert_ne!(first, second);
    }

    #[test]
    fn rejects_empty_and_relative_absolute_path_construction() {
        let resolver = SessionPathResolver::new(root());
        assert_eq!(resolver.resolve(""), Err(PathResolutionError::Empty));
        assert!(matches!(
            AbsolutePath::new("relative/path"),
            Err(PathResolutionError::NotAbsolute { .. })
        ));
    }

    #[test]
    fn serializes_as_the_full_platform_path() {
        assert_eq!(
            serde_json::to_value(root()).expect("serialize path"),
            serde_json::Value::String(root().as_str().to_owned())
        );
    }
}
