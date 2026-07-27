//! 文件能力契约：读取、列目录、内容搜索、写入、删除与局部编辑（edit）。
//!
//! 契约只描述能力形状与错误语义，不访问 `std::fs`/`tokio::fs`；真实路径解析、
//! 授权根、符号链接策略、大小限制、确认和审计归 Runtime/Adapter 实现侧。
//!
//! 已固化的契约语义（fake 与真实实现必须同构，桥接工具零格式化）：
//!
//! - `read` 返回的 `content` 每行带 1 起始行号，格式为 `{行号}\t{行文本}`；
//! - `search` 的 `query` 是字面量子串（本版本契约不含正则/通配，富语法属实现
//!   侧增强）；`grep`/`rg` 语义归入 [`SearchKind::ByContent`]，`rg` 仅可作为
//!   实现侧内部后端；
//! - `edit` 为精确替换：`old_string` 缺失、多处匹配（未设 `replace_all`）、
//!   `old_string == new_string` 都是稳定错误。

use std::{future::Future, pin::Pin};

use serde::Serialize;
use thiserror::Error;

/// 文件能力方法的 Future。
pub type FsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, FileToolError>> + Send + 'a>>;

/// 文件能力失败分类；路径越界、审批等安全约束不在契约内（经授权接缝处理）。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FileToolError {
    /// 目标路径不存在（含写入时父目录不存在）。
    #[error("path not found: {path}")]
    NotFound {
        /// 缺失的路径。
        path: String,
    },
    /// 请求参数违反契约（空查询、edit 多处匹配、`old == new` 等）。
    #[error("invalid input: {message}")]
    InvalidInput {
        /// 模型可读的失败原因。
        message: String,
    },
    /// 底层 I/O 失败。
    #[error("io error: {message}")]
    Io {
        /// 模型可读的失败原因。
        message: String,
    },
}

impl FileToolError {
    /// 构造参数违反契约的错误。
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    /// 构造底层 I/O 失败。
    pub fn io(message: impl Into<String>) -> Self {
        Self::Io {
            message: message.into(),
        }
    }
}

/// 读取文件请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadFileRequest {
    /// 目标文件路径。
    pub path: String,
    /// 起始行号（1 起始，含）；缺省从第 1 行开始。
    pub offset: Option<u32>,
    /// 最多返回行数；缺省返回到文件尾。
    pub limit: Option<u32>,
}

/// 读取文件结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadFileResult {
    /// 带 1 起始行号的内容，每行格式为 `{行号}\t{行文本}`。
    pub content: String,
    /// 起始行之后仍有未返回的行。
    pub truncated: bool,
}

/// 目录条目类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryKind {
    /// 普通文件。
    File,
    /// 目录。
    Directory,
    /// 其他（符号链接、设备等）。
    Other,
}

/// 目录条目。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileEntry {
    /// 条目名（不含父路径）。
    pub name: String,
    /// 条目类型。
    pub kind: FileEntryKind,
}

/// 列目录请求（非递归）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListDirectoryRequest {
    /// 目标目录路径。
    pub path: String,
}

/// 列目录结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListDirectoryResult {
    /// 直接子条目，按名称字典序稳定排序。
    pub entries: Vec<FileEntry>,
}

/// 搜索方式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchKind {
    /// 按文件名（最后一个路径分量）子串匹配。
    ByName,
    /// 按文件内容逐行子串匹配。
    ByContent,
}

/// 搜索文件请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchFilesRequest {
    /// 字面量子串；空串是 `InvalidInput`。
    pub query: String,
    /// 搜索起始路径；缺省为能力根。
    pub path: Option<String>,
    /// 按名或按内容。
    pub kind: SearchKind,
    /// 最多返回匹配数；缺省由实现侧给定。
    pub max_results: Option<u32>,
}

/// 一条搜索匹配。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchMatch {
    /// 按名匹配。
    Name {
        /// 匹配文件路径。
        path: String,
    },
    /// 按内容匹配。
    Content {
        /// 匹配文件路径。
        path: String,
        /// 匹配行号（1 起始）。
        line_number: u32,
        /// 匹配行原文。
        line: String,
    },
}

/// 搜索文件结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchFilesResult {
    /// 匹配列表，按路径字典序稳定排序。
    pub matches: Vec<SearchMatch>,
    /// 因 `max_results` 截断，仍有更多匹配。
    pub truncated: bool,
}

/// 写入文件请求：覆盖已存在文件或新建文件；父目录必须存在。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteFileRequest {
    /// 目标文件路径。
    pub path: String,
    /// 完整新内容。
    pub content: String,
}

/// 写入文件结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteFileResult {
    /// 写入字节数。
    pub bytes_written: u64,
}

/// 删除文件请求：删除文件或空目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeleteFileRequest {
    /// 目标路径。
    pub path: String,
}

/// 删除文件结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeleteFileResult {
    /// 被删除的路径。
    pub deleted: String,
}

/// 局部编辑请求：精确字符串替换。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditFileRequest {
    /// 目标文件路径。
    pub path: String,
    /// 被替换的原文；为空或与 `new_string` 相同是 `InvalidInput`。
    pub old_string: String,
    /// 替换后的新内容。
    pub new_string: String,
    /// 替换全部匹配；缺省要求唯一匹配。
    pub replace_all: bool,
}

/// 局部编辑结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditFileResult {
    /// 实际替换次数。
    pub replacements: u64,
}

/// Provider-neutral 的文件能力。
///
/// 路径为相对能力根的 `/` 分隔路径；路径越界、授权与真实解析策略不在契约内。
pub trait FileSystemTool: Send + Sync {
    /// 读取文件，内容带 1 起始行号。
    fn read<'a>(&'a self, request: ReadFileRequest) -> FsFuture<'a, ReadFileResult>;

    /// 非递归列目录。
    fn list<'a>(&'a self, request: ListDirectoryRequest) -> FsFuture<'a, ListDirectoryResult>;

    /// 按名或按内容搜索文件。
    fn search<'a>(&'a self, request: SearchFilesRequest) -> FsFuture<'a, SearchFilesResult>;

    /// 覆盖写入或新建文件。
    fn write<'a>(&'a self, request: WriteFileRequest) -> FsFuture<'a, WriteFileResult>;

    /// 删除文件或空目录。
    fn delete<'a>(&'a self, request: DeleteFileRequest) -> FsFuture<'a, DeleteFileResult>;

    /// 精确字符串替换编辑。
    fn edit<'a>(&'a self, request: EditFileRequest) -> FsFuture<'a, EditFileResult>;
}

/// 契约级分页与行号格式化：fake 与真实实现共用，保证 `read` 语义同构。
///
/// 返回 `(content, truncated)`；`offset` 超出总行数时返回空内容与 `false`。
pub fn paginate_with_line_numbers(
    text: &str,
    offset: Option<u32>,
    limit: Option<u32>,
) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let start = offset.unwrap_or(1).max(1) as usize;
    if start > lines.len() {
        return (String::new(), false);
    }
    let available = &lines[start - 1..];
    let take = limit.map_or(available.len(), |limit| limit as usize);
    let taken = &available[..take.min(available.len())];
    let mut content = String::new();
    for (index, line) in taken.iter().enumerate() {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(&format!("{}\t{}", start + index, line));
    }
    let truncated = take < available.len();
    (content, truncated)
}

/// 契约级精确替换：fake 与真实实现共用，保证 `edit` 错误语义同构。
///
/// 返回 `(新内容, 替换次数)`；`old` 为空、`old == new`、无匹配、未设
/// `replace_all` 而多处匹配都返回 [`FileToolError::InvalidInput`]。
pub fn exact_replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, u64), FileToolError> {
    if old.is_empty() {
        return Err(FileToolError::invalid_input(
            "old_string must not be empty".to_owned(),
        ));
    }
    if old == new {
        return Err(FileToolError::invalid_input(
            "old_string and new_string must differ".to_owned(),
        ));
    }
    let matches = content.matches(old).count() as u64;
    if matches == 0 {
        return Err(FileToolError::invalid_input(
            "old_string not found in file".to_owned(),
        ));
    }
    if matches > 1 && !replace_all {
        return Err(FileToolError::invalid_input(format!(
            "old_string matched {matches} times; set replace_all to replace every occurrence"
        )));
    }
    let replaced = if replace_all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    };
    Ok((replaced, matches))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginate_adds_one_based_line_numbers() {
        let (content, truncated) = paginate_with_line_numbers("a\nb\nc", None, None);
        assert_eq!(content, "1\ta\n2\tb\n3\tc");
        assert!(!truncated);
    }

    #[test]
    fn paginate_offsets_limits_and_marks_truncation() {
        let (content, truncated) = paginate_with_line_numbers("a\nb\nc\nd", Some(2), Some(2));
        assert_eq!(content, "2\tb\n3\tc");
        assert!(truncated);

        let (content, truncated) = paginate_with_line_numbers("a\nb", Some(5), None);
        assert_eq!(content, "");
        assert!(!truncated);
    }

    #[test]
    fn exact_replace_enforces_contract_errors() {
        assert!(exact_replace("abc", "", "x", false).is_err());
        assert!(exact_replace("abc", "a", "a", false).is_err());
        assert!(exact_replace("abc", "z", "x", false).is_err());
        assert!(exact_replace("aba", "a", "x", false).is_err());

        let (text, count) = exact_replace("aba", "a", "x", true).expect("replace_all");
        assert_eq!((text.as_str(), count), ("xbx", 2));

        let (text, count) = exact_replace("abc", "b", "x", false).expect("unique");
        assert_eq!((text.as_str(), count), ("axc", 1));
    }
}
