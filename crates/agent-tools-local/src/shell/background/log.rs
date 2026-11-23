//! 有界尾部日志。文件和字符位置由同一把锁保护。

use agent_tools::{
    BackgroundOutput, BackgroundOutputPage, BackgroundOutputRequest, BackgroundShellFuture,
    ShellOutputChannel, ShellToolError,
};
use std::{
    collections::VecDeque,
    io::{self, SeekFrom},
    path::{Path, PathBuf},
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, mpsc},
};

use super::super::output::{IncrementalUtf8Decoder, RawOutput};

const MAX_BYTES: u64 = 100 * 1024 * 1024;
const ROTATION_RESERVE: u64 = MAX_BYTES / 4;
const CHECKPOINT_INTERVAL: u64 = 64 * 1024;
const COPY_BUFFER_BYTES: usize = 256 * 1024;

pub(super) struct Log {
    path: PathBuf,
    state: Mutex<Option<RetainedLog>>,
}

struct RetainedLog {
    file: File,
    /// 稀疏检查点按字节和字符递增。首项就是文件开头，滚动后仍使用绝对位置。
    checkpoints: VecDeque<Position>,
    end: Position,
    write_state: WriteState,
}

#[derive(Clone, Copy, Default)]
struct Position {
    bytes: u64,
    chars: u64,
}

enum WriteState {
    Ready,
    Writing,
    Failed(io::Error),
}

impl Log {
    pub(super) async fn open(path: PathBuf) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path).await?;
        Ok(Self {
            path,
            state: Mutex::new(Some(RetainedLog {
                file,
                checkpoints: VecDeque::from([Position::default()]),
                end: Position::default(),
                write_state: WriteState::Ready,
            })),
        })
    }

    pub(super) async fn collect(&self, mut receiver: mpsc::Receiver<RawOutput>) {
        let mut stdout = IncrementalUtf8Decoder::default();
        let mut stderr = IncrementalUtf8Decoder::default();
        while let Some(output) = receiver.recv().await {
            let decoded = match output.channel {
                ShellOutputChannel::Stdout => stdout.push(&output.bytes),
                ShellOutputChannel::Stderr => stderr.push(&output.bytes),
            };
            self.append(&decoded).await;
        }
        self.append(&stdout.finish()).await;
        self.append(&stderr.finish()).await;
    }

    async fn append(&self, decoded: &str) {
        let mut state = self.state.lock().await;
        if let Some(log) = state.as_mut()
            && !decoded.is_empty()
            && matches!(log.write_state, WriteState::Ready)
        {
            // 进程收尾可中止写入任务。写入或搬移未完成时，读取不能使用旧索引。
            log.write_state = WriteState::Writing;
            // 写失败后继续排空双流，避免后台进程被输出反压卡住。
            log.write_state = match log.append(decoded).await {
                Ok(()) => WriteState::Ready,
                Err(error) => WriteState::Failed(error),
            };
        }
    }

    pub(super) async fn close(&self) -> Result<BackgroundOutputPage, ShellToolError> {
        let mut log = self.state.lock().await.take().ok_or_else(closed)?;
        log.page(BackgroundOutputRequest {
            cursor: None,
            offset_chars: 0,
            limit_chars: 2_000,
        })
        .await
    }
}

impl BackgroundOutput for Log {
    fn path(&self) -> &Path {
        &self.path
    }

    fn is_available(&self) -> bool {
        self.state.try_lock().map_or(true, |state| {
            state
                .as_ref()
                .is_some_and(|log| matches!(log.write_state, WriteState::Ready))
        })
    }

    fn read(
        &self,
        request: BackgroundOutputRequest,
    ) -> BackgroundShellFuture<'_, BackgroundOutputPage> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .as_mut()
                .ok_or_else(closed)?
                .page(request)
                .await
        })
    }
}

impl RetainedLog {
    async fn append(&mut self, decoded: &str) -> io::Result<()> {
        let mut start = self.checkpoints[0];
        let next_end = Position {
            bytes: self.end.bytes + decoded.len() as u64,
            chars: self.end.chars + decoded.chars().count() as u64,
        };
        if next_end.bytes - start.bytes > MAX_BYTES {
            let discard_before = next_end.bytes - (MAX_BYTES - ROTATION_RESERVE);
            let discard = self
                .checkpoints
                .partition_point(|position| position.bytes < discard_before);
            let new_start = self.checkpoints.get(discard).copied().unwrap_or(self.end);
            // 分批移除最旧输出。按检查点截断保证 UTF-8 完整，且无需扫描被淘汰的正文。
            self.compact(
                new_start.bytes - start.bytes,
                self.end.bytes - new_start.bytes,
            )
            .await?;
            self.checkpoints.drain(..discard);
            if self.checkpoints.is_empty() {
                self.checkpoints.push_back(new_start);
            }
            start = new_start;
        }
        self.file
            .seek(SeekFrom::Start(self.end.bytes - start.bytes))
            .await?;
        self.file.write_all(decoded.as_bytes()).await?;
        self.file.flush().await?;
        self.end = next_end;
        // 解码片段有界，每个检查点与前一项最多相隔一个区间加一个解码片段。
        let last = self.checkpoints[self.checkpoints.len() - 1];
        if next_end.bytes - last.bytes >= CHECKPOINT_INTERVAL {
            self.checkpoints.push_back(next_end);
        }
        Ok(())
    }

    async fn compact(&mut self, removed: u64, retained: u64) -> io::Result<()> {
        let mut buffer = vec![0; COPY_BUFFER_BYTES];
        let mut moved = 0;
        while moved < retained {
            let size = (retained - moved).min(COPY_BUFFER_BYTES as u64) as usize;
            let block = &mut buffer[..size];
            // 从前往后向文件头搬移。先读完整块再写，源区间尚未读取的部分不会被覆盖。
            self.file.seek(SeekFrom::Start(removed + moved)).await?;
            self.file.read_exact(block).await?;
            self.file.seek(SeekFrom::Start(moved)).await?;
            self.file.write_all(block).await?;
            moved += size as u64;
        }
        self.file.flush().await?;
        self.file.set_len(retained).await
    }

    async fn page(
        &mut self,
        request: BackgroundOutputRequest,
    ) -> Result<BackgroundOutputPage, ShellToolError> {
        match &self.write_state {
            WriteState::Ready => {}
            WriteState::Writing => return Err(read_error("output write interrupted")),
            WriteState::Failed(error) => return Err(read_error(error)),
        }
        if !(1..=8_000).contains(&request.limit_chars) {
            return Err(invalid("limit_chars must be between 1 and 8000"));
        }
        let start = self.checkpoints[0];
        let requested = match request.cursor {
            Some(cursor) => cursor
                .checked_add_signed(request.offset_chars)
                .filter(|position| *position <= self.end.chars)
                .ok_or_else(|| invalid("requested cursor is outside the output"))?,
            None if request.offset_chars != 0 => {
                return Err(invalid("offset_chars requires a cursor"));
            }
            None => self
                .end
                .chars
                .saturating_sub(request.limit_chars as u64)
                .max(start.chars),
        };
        let cursor = requested.max(start.chars);
        let next_cursor = cursor
            .saturating_add(request.limit_chars as u64)
            .min(self.end.chars);
        let content = if cursor == next_cursor {
            String::new()
        } else {
            let first = self
                .checkpoints
                .partition_point(|position| position.chars <= cursor);
            let last = self
                .checkpoints
                .partition_point(|position| position.chars < next_cursor);
            let begin = self.checkpoints[first - 1];
            let end = self.checkpoints.get(last).copied().unwrap_or(self.end);
            // 只读覆盖请求页的检查点区间，最多多读页首和页尾各一个检查点区间。
            let mut buffer = vec![0; (end.bytes - begin.bytes) as usize];
            self.file
                .seek(SeekFrom::Start(begin.bytes - start.bytes))
                .await
                .map_err(read_error)?;
            self.file
                .read_exact(&mut buffer)
                .await
                .map_err(read_error)?;
            let decoded = std::str::from_utf8(&buffer).map_err(read_error)?;
            let size = (next_cursor - cursor) as usize;
            let content: String = decoded
                .chars()
                .skip((cursor - begin.chars) as usize)
                .take(size)
                .collect();
            if content.chars().count() != size {
                return Err(read_error("output is shorter than indexed"));
            }
            content
        };
        Ok(BackgroundOutputPage {
            content,
            cursor,
            next_cursor,
            dropped_chars: cursor - requested,
        })
    }
}

fn read_error(error: impl std::fmt::Display) -> ShellToolError {
    ShellToolError::Io {
        message: format!("background log unavailable: {error}"),
    }
}

fn closed() -> ShellToolError {
    ShellToolError::BackgroundOutputClosed
}

fn invalid(message: &str) -> ShellToolError {
    ShellToolError::InvalidInput {
        message: message.into(),
    }
}
