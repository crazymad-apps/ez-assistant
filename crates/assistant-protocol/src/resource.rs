//! Session 文件资源的跨层定位与有界读取契约。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Session 创建时冻结的文件根身份；协议不传输根的物理路径。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionResourceRoot {
    WorkspacePrimary,
    WorkspaceAdditional { directory_index: u32 },
    SessionPrivate,
}

/// 根内资源定位。空相对路径表示根本身。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionResourceLocator {
    pub root: SessionResourceRoot,
    pub relative_path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionResourceEntryKind {
    Directory,
    File,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionResourceEntryState {
    Available,
    OutsideRoot,
    Unsupported,
}

/// 一层目录项；符号链接保留自身身份，同时公开最终目标是否仍在根内。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct SessionResourceEntry {
    pub locator: SessionResourceLocator,
    pub display_name: String,
    pub kind: SessionResourceEntryKind,
    pub state: SessionResourceEntryState,
    pub is_symbolic_link: bool,
    pub is_hidden: bool,
    pub is_generated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListSessionResourceFilesRequest {
    pub locator: SessionResourceLocator,
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default)]
    pub include_generated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct ListSessionResourceFilesResult {
    pub entries: Vec<SessionResourceEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreviewSessionResourceFileRequest {
    pub locator: SessionResourceLocator,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
#[serde(rename_all = "snake_case")]
pub enum SessionResourcePreviewKind {
    Text,
    Image,
    Pdf,
}

/// Host 返回给受信任 Desktop 桥的有界预览；图片与 PDF 正文使用 base64，WebView 不接触物理路径。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[ts(export_to = "assistant-protocol.ts")]
pub struct PreviewSessionResourceFileResult {
    pub kind: SessionResourcePreviewKind,
    pub media_type: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data_base64: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn additional_root_uses_stable_tagged_identity() {
        let value = serde_json::to_value(SessionResourceLocator {
            root: SessionResourceRoot::WorkspaceAdditional { directory_index: 2 },
            relative_path: "src/lib.rs".to_owned(),
        })
        .expect("resource locator should serialize");
        assert_eq!(value["root"]["type"], "workspace_additional");
        assert_eq!(value["root"]["directory_index"], 2);
        assert_eq!(value["relative_path"], "src/lib.rs");
    }
}
