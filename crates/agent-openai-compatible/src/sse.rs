//! SSE（Server-Sent Events）frame 解析：把字节流增量解析为 data frame。
//!
//! 解析语义遵循 SSE 规范：空行触发分发；同一 frame 的多行 `data:` 用 `\n`
//! 合并；`:` 开头的行是心跳/注释，忽略；`event:`/`id:`/`retry:` 及未知字段
//! 忽略但不报错。`data: [DONE]` 不做特判，由 Adapter 识别处理。

use agent_model::ModelError;

#[derive(Clone, Debug, Eq, PartialEq)]
/// 一个 SSE frame 的 data 负载；同一 frame 的多行 `data:` 已用 `\n` 合并。
pub struct SseFrame {
    /// 合并后的 data 文本。
    pub data: String,
}

/// 增量 SSE 解析器。
///
/// 逐段喂入字节，每次返回本次完整解析出的 frames；跨段的半行字节缓冲到
/// 下一段。字节流在事件中途结束时，未分发的残余按 SSE 规范丢弃。
pub struct SseParser {
    /// 尚未遇到换行符的残余字节。
    pending: Vec<u8>,
    /// 当前事件已累积的 data 行。
    data_lines: Vec<String>,
}

impl SseParser {
    /// 创建空解析器。
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            data_lines: Vec::new(),
        }
    }

    /// 喂入一段字节，返回本次完整解析出的 frames。
    ///
    /// 行内容必须是合法 UTF-8，否则以 [`ModelError::Protocol`] 失败。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ModelError> {
        self.pending.extend_from_slice(chunk);
        let mut frames = Vec::new();
        // `\n` 不会出现在多字节 UTF-8 序列内部，按字节找行边界是安全的。
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = self.pending.drain(..=position).collect();
            let mut line = &raw[..raw.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            self.process_line(line, &mut frames)?;
        }
        Ok(frames)
    }

    /// 处理一行：空行分发、注释忽略、字段累积。
    fn process_line(&mut self, line: &[u8], frames: &mut Vec<SseFrame>) -> Result<(), ModelError> {
        // 空行：分发已累积的 data；没有 data 的事件（纯心跳等）直接丢弃。
        if line.is_empty() {
            if !self.data_lines.is_empty() {
                frames.push(SseFrame {
                    data: std::mem::take(&mut self.data_lines).join("\n"),
                });
            }
            return Ok(());
        }
        // `:` 开头的行是心跳/注释。
        if line[0] == b':' {
            return Ok(());
        }
        let text = std::str::from_utf8(line).map_err(|error| {
            ModelError::Protocol(format!("sse line is not valid utf-8: {error}"))
        })?;
        // 无冒号的行按规范视为值为空的字段。
        let (field, value) = match text.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (text, ""),
        };
        if field == "data" {
            self.data_lines.push(value.to_owned());
        }
        // event:/id:/retry: 及未知字段按规范忽略。
        Ok(())
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}
