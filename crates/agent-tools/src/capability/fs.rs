//! 文件能力契约：读取、列目录、按名查找、内容搜索、写入、删除与局部编辑。
//!
//! 请求路径已经由标准工具壳解析成 [`AbsolutePath`]；能力实现不再猜测默认工作目录。
//! 本模块只描述能力形状与稳定错误，不访问真实文件系统，也不包含权限或审批规则。

use std::{
    future::Future,
    num::{NonZeroU32, NonZeroU64},
    pin::Pin,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::AbsolutePath;

/// 文件能力方法的 Future。
pub type FsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, FileToolError>> + Send + 'a>>;

/// 单次文件能力执行的控制上下文。
#[derive(Clone, Debug)]
pub struct FileToolContext {
    /// 取消信号；实现收到取消后必须完成必要清理，edit 保留逐文件结果，其他操作返回取消错误。
    pub cancellation: CancellationToken,
}

impl FileToolContext {
    /// 用执行级取消信号创建上下文。
    pub fn new(cancellation: CancellationToken) -> Self {
        Self { cancellation }
    }
}

impl Default for FileToolContext {
    fn default() -> Self {
        Self::new(CancellationToken::new())
    }
}

/// 文件调用的授权操作类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    List,
    Find,
    Search,
    Write,
    Edit,
    Delete,
}

/// 文件策略读取的类型化 resolved 事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileAuthorizationFacts {
    /// 当前标准工具壳对应的文件操作。
    pub operation: FileOperation,
    /// 已按 Session 工作目录词法归一化的绝对逻辑路径。
    pub path: AbsolutePath,
}

/// 一次工具调用涉及多条路径时提供的类型化授权事实。
///
/// 多路径不能压缩成共同父目录，否则会把实际操作范围错误扩大为目录授权。Runtime 必须逐条
/// 应用文件规则，并且只有全部路径均获准时才允许执行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileBatchAuthorizationFacts {
    /// 当前工具壳对应的文件操作（批量读取或编辑）。
    pub operation: FileOperation,
    /// 已按 Session 工作目录分别完成词法归一化、保持模型输入顺序的绝对逻辑路径。
    pub paths: Vec<AbsolutePath>,
}

/// 文件能力失败分类；路径越界、审批等安全约束不在能力契约内。
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FileToolError {
    #[error("path not found: {path}")]
    NotFound { path: AbsolutePath },
    #[error("invalid input: {message}")]
    InvalidInput { message: String },
    #[error("io error: {message}")]
    Io { message: String },
    #[error("unsupported text encoding: {path}")]
    UnsupportedEncoding { path: AbsolutePath },
    #[error("unsupported file type: {path}")]
    UnsupportedFileType { path: AbsolutePath },
    #[error("search backend unavailable: {message}")]
    SearchBackendUnavailable { message: String },
    #[error("file operation cancelled")]
    Cancelled,
    #[error("file changed while editing: {path}")]
    ConcurrentModification { path: AbsolutePath },
    #[error("file is too large: {path} ({actual_bytes} bytes, maximum {maximum_bytes} bytes)")]
    TooLarge {
        path: AbsolutePath,
        actual_bytes: u64,
        maximum_bytes: NonZeroU64,
    },
}

impl FileToolError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Io {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadFileRequest {
    /// 已解析的目标绝对逻辑路径。
    pub path: AbsolutePath,
    /// 起始行号（1 起始，含）。
    pub offset: NonZeroU32,
    /// 最多返回行数。
    pub limit: NonZeroU32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadFileResult {
    /// 实际读取的绝对逻辑路径。
    pub path: AbsolutePath,
    /// 本页采用的 1 起始首行行号。
    pub offset: NonZeroU32,
    /// 本页采用的最大行数。
    pub limit: NonZeroU32,
    /// 带 1 起始行号的内容，每行格式为 `{行号}\t{行文本}`。
    pub content: String,
    /// 下一页起始行；`None` 表示已经到达文件尾。
    pub next_offset: Option<NonZeroU32>,
    /// 是否还有内容因分页上限未返回。
    pub truncated: bool,
}

/// 目录条目的目标类型；是否为符号链接由 [`FileEntry::is_symlink`] 正交表达。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryKind {
    File,
    Directory,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileEntry {
    /// 条目的完整绝对逻辑路径。
    pub path: AbsolutePath,
    /// 跟随链接后或实现可判断的目标类型。
    pub kind: FileEntryKind,
    /// 目录项自身是否为符号链接；可与 File/Directory 同时为真。
    pub is_symlink: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListDirectoryRequest {
    /// 需要列出的绝对逻辑目录路径。
    pub path: AbsolutePath,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListDirectoryResult {
    /// 直接子条目，按完整路径稳定排序。
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    ByName,
    ByContent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchFilesRequest {
    /// 字面量子串；空串是 `InvalidInput`。
    pub query: String,
    /// 搜索起始绝对逻辑路径。
    pub path: AbsolutePath,
    /// 按文件名还是按文件内容搜索。
    pub kind: SearchKind,
    /// 最多返回的匹配数。
    pub max_results: NonZeroU32,
    /// 单次搜索允许读取的 stdout 总字节数。
    pub max_output_bytes: NonZeroU64,
    /// 单条 NUL/JSON 记录允许读取的最大字节数。
    pub max_record_bytes: NonZeroU64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchMatch {
    Name {
        path: AbsolutePath,
    },
    Content {
        path: AbsolutePath,
        /// 匹配行号（1 起始）。
        line_number: NonZeroU32,
        line: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchFilesResult {
    /// 按实现定义的稳定顺序返回的匹配项。
    pub matches: Vec<SearchMatch>,
    /// 是否因任一配置上限而只返回了部分结果。
    pub truncated: bool,
    /// 截断时的稳定原因；未截断时为 `None`。
    pub truncation_reason: Option<SearchTruncationReason>,
}

/// 搜索主动停止并返回部分结果的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchTruncationReason {
    /// 返回匹配数达到上限。
    MaxResults,
    /// 搜索后端 stdout 的累计读取量达到上限。
    MaxOutputBytes,
    /// 单条 NUL/JSON 记录超过上限，未解析该记录。
    OversizedRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteFileRequest {
    /// 需要创建或覆盖的绝对逻辑路径。
    pub path: AbsolutePath,
    /// 需要写入的 UTF-8 文本。
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteFileResult {
    /// 实际写入的绝对逻辑路径。
    pub path: AbsolutePath,
    /// 写入内容的 UTF-8 字节数。
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeleteFileRequest {
    /// 需要删除的文件、符号链接或空目录路径。
    pub path: AbsolutePath,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeleteFileResult {
    /// 已删除节点的绝对逻辑路径。
    pub deleted: AbsolutePath,
}

/// 批量编辑固定契约限额，执行入口和工具 resolve 共用。
pub const MAX_EDIT_FILES: usize = 16;
pub const MAX_EDITS_PER_FILE: usize = 64;
pub const MAX_EDIT_GROUPS: usize = 128;
pub const MAX_EDIT_INPUT_BYTES: usize = 256 * 1024;

/// 一组精确替换；按输入顺序作用于上一组的内存结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditReplacement {
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    #[schemars(default)]
    pub replace_all: bool,
}

/// 单文件的全部替换组；任何组失败均不提交该文件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditFileEntry {
    pub path: AbsolutePath,
    pub edits: Vec<EditReplacement>,
}

/// 已解析的整批编辑请求；能力实现必须再次核对限额。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditFileRequest {
    pub files: Vec<EditFileEntry>,
}

impl EditFileRequest {
    /// 校验冻结请求的数量、参数及完整路径/正文预算，不访问文件系统。
    ///
    /// # Errors
    /// 超出契约预算或含无效替换时返回 InvalidInput，整批不得执行。
    pub fn validate(&self) -> Result<(), FileToolError> {
        if self.files.is_empty() || self.files.len() > MAX_EDIT_FILES {
            return Err(FileToolError::invalid_input(
                "files must contain 1..16 entries",
            ));
        }
        let mut groups = 0;
        let mut path_bytes = 0;
        let mut text_json_bytes = 0;
        let mut paths = Vec::new();
        for file in &self.files {
            if file.edits.is_empty() || file.edits.len() > MAX_EDITS_PER_FILE {
                return Err(FileToolError::invalid_input(
                    "edits must contain 1..64 entries",
                ));
            }
            groups += file.edits.len();
            if file.path.as_str().len() > 1024 {
                return Err(FileToolError::invalid_input("resolved path exceeds 1 KiB"));
            }
            path_bytes += file.path.as_str().len();
            paths.push(file.path.as_str());
            for edit in &file.edits {
                // 原始完整 JSON 的 256 KiB 由输入反序列化核对。这里独立限制正文，
                // 不把默认值补齐/绝对路径展开增加的字节误算为模型原始输入。
                for text in [&edit.old_string, &edit.new_string] {
                    text_json_bytes += serde_json::to_vec(text)
                        .map_err(|_| FileToolError::invalid_input("invalid edit text"))?
                        .len();
                }
                if edit.old_string.is_empty() || edit.old_string == edit.new_string {
                    return Err(FileToolError::invalid_input(
                        "old_string must be nonempty and differ from new_string",
                    ));
                }
            }
        }
        if groups > MAX_EDIT_GROUPS || path_bytes > 4096 {
            return Err(FileToolError::invalid_input(
                "edit group or path budget exceeded",
            ));
        }
        if serde_json::to_vec(&paths).map_or(true, |json| json.len() > 8192)
            || text_json_bytes > MAX_EDIT_INPUT_BYTES
        {
            return Err(FileToolError::invalid_input("edit JSON budget exceeded"));
        }
        Ok(())
    }
}

/// 尚未执行的文件位置原因；不代表该文件已经核验或提交。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditNotExecutedReason {
    Cancelled,
    PreviousFailure,
    PreflightFailed,
}

/// 逐文件提交事实；只有完整提交才携带完成替换次数。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EditFileOutcome {
    Success {
        replacements: u64,
    },
    Failed {
        code: String,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        edit_index: Option<usize>,
    },
    Unknown {
        code: String,
        reason: String,
    },
    NotExecuted {
        reason: EditNotExecutedReason,
    },
}

impl EditFileOutcome {
    /// 未开始文件写入时形成稳定失败；不重复路径和替换正文。
    pub fn failed(error: &FileToolError, edit_index: Option<usize>) -> Self {
        let (code, reason) = match error {
            FileToolError::InvalidInput { message } => ("invalid_input", message.as_str()),
            FileToolError::NotFound { .. } => ("not_found", "file not found"),
            FileToolError::UnsupportedEncoding { .. } => {
                ("unsupported_encoding", "file is not supported UTF-8 text")
            }
            FileToolError::UnsupportedFileType { .. } => {
                ("unsupported_file_type", "target must be a regular file")
            }
            FileToolError::TooLarge { .. } => {
                ("too_large", "file exceeds the configured size limit")
            }
            FileToolError::ConcurrentModification { .. } => {
                ("concurrent_modification", "file changed before commit")
            }
            FileToolError::Cancelled => ("cancelled", "cancelled before commit"),
            _ => ("io", "file I/O could not be completed"),
        };
        let mut end = reason.len().min(256);
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        Self::Failed {
            code: code.to_owned(),
            reason: reason[..end].to_owned(),
            edit_index,
        }
    }
}

/// 每条输入位置的结果；路径为授权时冻结的逻辑路径。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditFileEntryResult {
    pub path: AbsolutePath,
    #[serde(flatten)]
    pub outcome: EditFileOutcome,
}

/// 完整批量结果，始终保持输入顺序和条目数；不做执行后裁剪。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EditFileResult {
    pub files: Vec<EditFileEntryResult>,
}

impl EditFileResult {
    /// 创建全部尚未执行的清单，执行时只替换已有输入位置。
    pub fn not_executed(request: &EditFileRequest, reason: EditNotExecutedReason) -> Self {
        Self {
            files: request
                .files
                .iter()
                .map(|file| EditFileEntryResult {
                    path: file.path.clone(),
                    outcome: EditFileOutcome::NotExecuted { reason },
                })
                .collect(),
        }
    }
}

/// Provider-neutral 文件能力；所有方法都接收显式取消上下文。
pub trait FileSystemTool: Send + Sync {
    fn read<'a>(
        &'a self,
        request: ReadFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ReadFileResult>;
    fn list<'a>(
        &'a self,
        request: ListDirectoryRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ListDirectoryResult>;
    fn search<'a>(
        &'a self,
        request: SearchFilesRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, SearchFilesResult>;
    fn write<'a>(
        &'a self,
        request: WriteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, WriteFileResult>;
    fn delete<'a>(
        &'a self,
        request: DeleteFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, DeleteFileResult>;
    fn edit<'a>(
        &'a self,
        request: EditFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, EditFileResult>;
}

/// 对文本按显式非零行号和行数分页，并生成下一页位置。
pub fn paginate_with_line_numbers(
    text: &str,
    offset: NonZeroU32,
    limit: NonZeroU32,
) -> (String, Option<NonZeroU32>, bool) {
    let lines: Vec<&str> = text.lines().collect();
    let start = offset.get() as usize;
    if start > lines.len() {
        return (String::new(), None, false);
    }
    let available = &lines[start - 1..];
    let take = limit.get() as usize;
    let taken = &available[..take.min(available.len())];
    let mut content = String::new();
    for (index, line) in taken.iter().enumerate() {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(&format!("{}\t{}", start + index, line));
    }
    let truncated = taken.len() < available.len();
    let next_offset = if truncated {
        u32::try_from(start + taken.len())
            .ok()
            .and_then(NonZeroU32::new)
    } else {
        None
    };
    (content, next_offset, truncated)
}

/// 契约级精确替换，供 fake 与真实 Adapter 复用。
pub fn exact_replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, u64), FileToolError> {
    if old.is_empty() {
        return Err(FileToolError::invalid_input("old_string must not be empty"));
    }
    if old == new {
        return Err(FileToolError::invalid_input(
            "old_string and new_string must differ",
        ));
    }
    let matches = content.matches(old).count() as u64;
    if matches == 0 {
        return Err(FileToolError::invalid_input("old_string not found in file"));
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

    fn nonzero(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("non-zero test value")
    }

    #[test]
    fn paginate_adds_line_numbers_and_next_offset() {
        let (content, next_offset, truncated) =
            paginate_with_line_numbers("a\nb\nc\nd", nonzero(2), nonzero(2));
        assert_eq!(content, "2\tb\n3\tc");
        assert_eq!(next_offset, Some(nonzero(4)));
        assert!(truncated);

        let (content, next_offset, truncated) =
            paginate_with_line_numbers("a\nb", nonzero(5), nonzero(2));
        assert!(content.is_empty());
        assert_eq!(next_offset, None);
        assert!(!truncated);
    }

    #[test]
    fn symlink_is_orthogonal_to_target_kind() {
        #[cfg(windows)]
        let path = AbsolutePath::new(r"C:\workspace\linked").expect("absolute path");
        #[cfg(not(windows))]
        let path = AbsolutePath::new("/workspace/linked").expect("absolute path");
        let entry = FileEntry {
            path,
            kind: FileEntryKind::Directory,
            is_symlink: true,
        };
        assert_eq!(entry.kind, FileEntryKind::Directory);
        assert!(entry.is_symlink);
    }

    #[test]
    fn exact_replace_enforces_contract_errors() {
        assert!(exact_replace("abc", "", "x", false).is_err());
        assert!(exact_replace("abc", "a", "a", false).is_err());
        assert!(exact_replace("abc", "z", "x", false).is_err());
        assert!(exact_replace("aba", "a", "x", false).is_err());
        assert_eq!(
            exact_replace("aba", "a", "x", true).expect("replace all"),
            ("xbx".to_owned(), 2)
        );
    }
}
