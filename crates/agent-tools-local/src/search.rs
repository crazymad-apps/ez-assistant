//! 直接启动系统 ripgrep 的名称和内容搜索实现。

use std::{
    ffi::{OsStr, OsString},
    io::ErrorKind,
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    process::Stdio,
};

use agent_tools::{
    AbsolutePath, FileToolError, SearchFilesRequest, SearchFilesResult, SearchKind, SearchMatch,
    SearchTruncationReason,
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    process::{ChildStdout, Command},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::process::{self, ManagedChild};

pub(crate) async fn run_with_stderr_limit(
    program: &OsStr,
    request: SearchFilesRequest,
    max_stderr_bytes: NonZeroU64,
    cancellation: CancellationToken,
) -> Result<SearchFilesResult, FileToolError> {
    if request.query.is_empty() {
        return Err(FileToolError::invalid_input("query must not be empty"));
    }
    if cancellation.is_cancelled() {
        return Err(FileToolError::Cancelled);
    }
    match request.kind {
        SearchKind::ByName => search_names(program, request, max_stderr_bytes, cancellation).await,
        SearchKind::ByContent => {
            search_content(program, request, max_stderr_bytes, cancellation).await
        }
    }
}

async fn search_names(
    program: &OsStr,
    request: SearchFilesRequest,
    max_stderr_bytes: NonZeroU64,
    cancellation: CancellationToken,
) -> Result<SearchFilesResult, FileToolError> {
    let RunningRipgrep {
        mut child,
        mut stdout,
        stderr_task,
    } = spawn_rg(
        program,
        [
            OsString::from("--no-config"),
            OsString::from("--files"),
            OsString::from("--null"),
            OsString::from("--"),
            request.path.as_path().as_os_str().to_owned(),
        ],
        max_stderr_bytes,
    )?;
    let mut matches = Vec::new();
    let mut total_bytes = 0_u64;
    loop {
        let record = tokio::select! {
            _ = cancellation.cancelled() => {
                terminate(&mut child, stderr_task).await?;
                return Err(FileToolError::Cancelled);
            }
            result = read_bounded_record(
                &mut stdout,
                0,
                request.max_record_bytes,
                request.max_output_bytes,
                &mut total_bytes,
            ) => {
                result.map_err(|error| FileToolError::io(format!("read ripgrep file output failed: {error}")))
            }
        };
        let record = match record {
            Ok(record) => record,
            Err(error) => {
                terminate(&mut child, stderr_task).await?;
                return Err(error);
            }
        };
        let mut path_bytes = match record {
            RecordRead::Eof => break,
            RecordRead::Limit(reason) => {
                terminate(&mut child, stderr_task).await?;
                return Ok(truncated(matches, reason));
            }
            RecordRead::Record(record) => record,
        };
        if path_bytes.last() == Some(&0) {
            path_bytes.pop();
        }
        let Ok(text) = std::str::from_utf8(&path_bytes) else {
            continue;
        };
        let path = match output_path(&request.path, text) {
            Ok(path) => path,
            Err(error) => {
                terminate(&mut child, stderr_task).await?;
                return Err(error);
            }
        };
        let matches_query = path
            .as_path()
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(&request.query));
        if !matches_query {
            continue;
        }
        matches.push(SearchMatch::Name { path });
        if matches.len() > request.max_results.get() as usize {
            matches.truncate(request.max_results.get() as usize);
            terminate(&mut child, stderr_task).await?;
            return Ok(truncated(matches, SearchTruncationReason::MaxResults));
        }
    }
    finish(child, stderr_task, cancellation).await?;
    Ok(SearchFilesResult {
        matches,
        truncated: false,
        truncation_reason: None,
    })
}

async fn search_content(
    program: &OsStr,
    request: SearchFilesRequest,
    max_stderr_bytes: NonZeroU64,
    cancellation: CancellationToken,
) -> Result<SearchFilesResult, FileToolError> {
    let RunningRipgrep {
        mut child,
        mut stdout,
        stderr_task,
    } = spawn_rg(
        program,
        [
            OsString::from("--no-config"),
            OsString::from("--json"),
            OsString::from("--fixed-strings"),
            OsString::from("--"),
            OsString::from(&request.query),
            request.path.as_path().as_os_str().to_owned(),
        ],
        max_stderr_bytes,
    )?;
    let mut matches = Vec::new();
    let mut total_bytes = 0_u64;
    loop {
        let record = tokio::select! {
            _ = cancellation.cancelled() => {
                terminate(&mut child, stderr_task).await?;
                return Err(FileToolError::Cancelled);
            }
            result = read_bounded_record(
                &mut stdout,
                b'\n',
                request.max_record_bytes,
                request.max_output_bytes,
                &mut total_bytes,
            ) => {
                result.map_err(|error| FileToolError::io(format!("read ripgrep JSON output failed: {error}")))
            }
        };
        let record = match record {
            Ok(record) => record,
            Err(error) => {
                terminate(&mut child, stderr_task).await?;
                return Err(error);
            }
        };
        let line = match record {
            RecordRead::Eof => break,
            RecordRead::Limit(reason) => {
                terminate(&mut child, stderr_task).await?;
                return Ok(truncated(matches, reason));
            }
            RecordRead::Record(record) => record,
        };
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(error) => {
                terminate(&mut child, stderr_task).await?;
                return Err(FileToolError::io(format!(
                    "parse ripgrep JSON output failed: {error}"
                )));
            }
        };
        let search_match = match parse_content_match(&request.path, &value) {
            Ok(Some(search_match)) => search_match,
            Ok(None) => continue,
            Err(error) => {
                terminate(&mut child, stderr_task).await?;
                return Err(error);
            }
        };
        matches.push(search_match);
        if matches.len() > request.max_results.get() as usize {
            matches.truncate(request.max_results.get() as usize);
            terminate(&mut child, stderr_task).await?;
            return Ok(truncated(matches, SearchTruncationReason::MaxResults));
        }
    }
    finish(child, stderr_task, cancellation).await?;
    Ok(SearchFilesResult {
        matches,
        truncated: false,
        truncation_reason: None,
    })
}

enum RecordRead {
    Eof,
    Record(Vec<u8>),
    Limit(SearchTruncationReason),
}

/// 使用 BufRead 的内部固定缓冲逐段复制，先检查上限再扩展目标记录。
async fn read_bounded_record(
    reader: &mut (impl AsyncBufRead + Unpin),
    delimiter: u8,
    max_record_bytes: NonZeroU64,
    max_output_bytes: NonZeroU64,
    total_bytes: &mut u64,
) -> std::io::Result<RecordRead> {
    let mut record = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if record.is_empty() {
                RecordRead::Eof
            } else {
                RecordRead::Record(record)
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == delimiter)
            .map_or(available.len(), |index| index + 1);
        let take_u64 = take as u64;
        if (record.len() as u64).saturating_add(take_u64) > max_record_bytes.get() {
            return Ok(RecordRead::Limit(SearchTruncationReason::OversizedRecord));
        }
        if total_bytes.saturating_add(take_u64) > max_output_bytes.get() {
            return Ok(RecordRead::Limit(SearchTruncationReason::MaxOutputBytes));
        }
        let found_delimiter = available[take - 1] == delimiter;
        record.extend_from_slice(&available[..take]);
        reader.consume(take);
        *total_bytes += take_u64;
        if found_delimiter {
            return Ok(RecordRead::Record(record));
        }
    }
}

fn truncated(matches: Vec<SearchMatch>, reason: SearchTruncationReason) -> SearchFilesResult {
    SearchFilesResult {
        matches,
        truncated: true,
        truncation_reason: Some(reason),
    }
}

struct RunningRipgrep {
    child: ManagedChild,
    stdout: BufReader<ChildStdout>,
    stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
}

fn spawn_rg(
    program: &OsStr,
    args: impl IntoIterator<Item = OsString>,
    max_stderr_bytes: NonZeroU64,
) -> Result<RunningRipgrep, FileToolError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process::spawn(command).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            FileToolError::SearchBackendUnavailable {
                message: format!(
                    "ripgrep program `{}` was not found",
                    program.to_string_lossy()
                ),
            }
        } else {
            FileToolError::io(format!("start ripgrep failed: {error}"))
        }
    })?;
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| FileToolError::io("ripgrep stdout pipe was not created"))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| FileToolError::io("ripgrep stderr pipe was not created"))?;
    let stderr_task = tokio::spawn(drain_limited(stderr, max_stderr_bytes.get()));
    Ok(RunningRipgrep {
        child,
        stdout: BufReader::new(stdout),
        stderr_task,
    })
}

async fn drain_limited(
    mut reader: impl AsyncRead + Unpin,
    maximum_bytes: u64,
) -> std::io::Result<Vec<u8>> {
    let maximum_bytes = usize::try_from(maximum_bytes).unwrap_or(usize::MAX);
    let mut retained = Vec::with_capacity(maximum_bytes.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = maximum_bytes.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

async fn finish(
    mut child: ManagedChild,
    stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    cancellation: CancellationToken,
) -> Result<(), FileToolError> {
    let status = tokio::select! {
        _ = cancellation.cancelled() => {
            terminate(&mut child, stderr_task).await?;
            return Err(FileToolError::Cancelled);
        }
        result = child.wait() => {
            result.map_err(|error| FileToolError::io(format!("wait for ripgrep failed: {error}")))?
        }
    };
    let stderr = collect_stderr(stderr_task).await?;
    match status.code() {
        Some(0 | 1) => Ok(()),
        code => Err(FileToolError::io(format!(
            "ripgrep exited with status {code:?}: {}",
            String::from_utf8_lossy(&stderr).trim()
        ))),
    }
}

async fn terminate(
    child: &mut ManagedChild,
    stderr_task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), FileToolError> {
    process::terminate_and_wait(child)
        .await
        .map_err(|error| FileToolError::io(format!("terminate ripgrep failed: {error}")))?;
    collect_stderr(stderr_task).await?;
    Ok(())
}

async fn collect_stderr(
    task: JoinHandle<Result<Vec<u8>, std::io::Error>>,
) -> Result<Vec<u8>, FileToolError> {
    task.await
        .map_err(|error| FileToolError::io(format!("ripgrep stderr task failed: {error}")))?
        .map_err(|error| FileToolError::io(format!("read ripgrep stderr failed: {error}")))
}

fn parse_content_match(
    root: &AbsolutePath,
    value: &Value,
) -> Result<Option<SearchMatch>, FileToolError> {
    if value.get("type").and_then(Value::as_str) != Some("match") {
        return Ok(None);
    }
    let Some(data) = value.get("data") else {
        return Ok(None);
    };
    let Some(path) = data
        .get("path")
        .and_then(|path| path.get("text"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let Some(text) = data
        .get("lines")
        .and_then(|lines| lines.get("text"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let Some(line_number) = data
        .get("line_number")
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .and_then(NonZeroU32::new)
    else {
        return Err(FileToolError::io(
            "ripgrep match contains an invalid line number",
        ));
    };
    Ok(Some(SearchMatch::Content {
        path: output_path(root, path)?,
        line_number,
        line: trim_line_ending(text).to_owned(),
    }))
}

fn output_path(root: &AbsolutePath, output: &str) -> Result<AbsolutePath, FileToolError> {
    let path = PathBuf::from(output);
    let path = if path.is_absolute() {
        path
    } else {
        root.as_path().join(path)
    };
    AbsolutePath::new(path)
        .map_err(|error| FileToolError::io(format!("ripgrep returned an invalid path: {error}")))
}

fn trim_line_ending(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU32, NonZeroU64},
        path::Path,
    };

    use tempfile::TempDir;

    use super::*;

    fn absolute(path: impl AsRef<Path>) -> AbsolutePath {
        AbsolutePath::new(path.as_ref().to_path_buf()).expect("absolute UTF-8 temp path")
    }

    async fn run(
        program: &OsStr,
        request: SearchFilesRequest,
        cancellation: CancellationToken,
    ) -> Result<SearchFilesResult, FileToolError> {
        super::run_with_stderr_limit(
            program,
            request,
            NonZeroU64::new(64 * 1024).expect("non-zero stderr limit"),
            cancellation,
        )
        .await
    }

    fn request(root: &Path, query: &str, kind: SearchKind, maximum: u32) -> SearchFilesRequest {
        SearchFilesRequest {
            query: query.to_owned(),
            path: absolute(root),
            kind,
            max_results: NonZeroU32::new(maximum).expect("non-zero"),
            max_output_bytes: NonZeroU64::new(1024 * 1024).expect("non-zero"),
            max_record_bytes: NonZeroU64::new(64 * 1024).expect("non-zero"),
        }
    }

    #[cfg(unix)]
    fn fake_rg(script: &str) -> (TempDir, OsString) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temp directory");
        let program = directory.path().join("rg-fake");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("write fake rg");
        let mut permissions = std::fs::metadata(&program)
            .expect("fake metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions).expect("make fake executable");
        (directory, program.into_os_string())
    }

    #[cfg(unix)]
    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_name_uses_literal_filter_and_marks_truncated() {
        let root = tempfile::tempdir().expect("search root");
        let paths = ["alpha.txt", "beta.txt", "alphabet.md"].map(|name| root.path().join(name));
        let script = format!(
            "printf '%s\\0%s\\0%s\\0' {} {} {}",
            shell_quote(paths[0].to_str().expect("UTF-8 path")),
            shell_quote(paths[1].to_str().expect("UTF-8 path")),
            shell_quote(paths[2].to_str().expect("UTF-8 path")),
        );
        let (_program_directory, program) = fake_rg(&script);
        let result = run(
            &program,
            request(root.path(), "alpha", SearchKind::ByName, 1),
            CancellationToken::new(),
        )
        .await
        .expect("search names");
        assert_eq!(result.matches.len(), 1);
        assert!(result.truncated);
        assert_eq!(
            result.truncation_reason,
            Some(SearchTruncationReason::MaxResults)
        );
        assert_eq!(
            result.matches[0],
            SearchMatch::Name {
                path: absolute(&paths[0])
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_content_parses_text_and_skips_binary_payloads() {
        let root = tempfile::tempdir().expect("search root");
        let text_path = root.path().join("long path.txt");
        let text_message = serde_json::json!({
            "type": "match",
            "data": {
                "path": {"text": text_path.to_str().expect("UTF-8 path")},
                "lines": {"text": "matching line\n"},
                "line_number": 7
            }
        });
        let binary_message = serde_json::json!({
            "type": "match",
            "data": {
                "path": {"text": text_path.to_str().expect("UTF-8 path")},
                "lines": {"bytes": "AA=="},
                "line_number": 8
            }
        });
        let script = format!(
            "printf '%s\\n%s\\n' {} {}",
            shell_quote(&text_message.to_string()),
            shell_quote(&binary_message.to_string()),
        );
        let (_program_directory, program) = fake_rg(&script);
        let result = run(
            &program,
            request(root.path(), "matching", SearchKind::ByContent, 10),
            CancellationToken::new(),
        )
        .await
        .expect("search content");
        assert_eq!(
            result.matches,
            [SearchMatch::Content {
                path: absolute(text_path),
                line_number: NonZeroU32::new(7).expect("non-zero"),
                line: "matching line".to_owned(),
            }]
        );
        assert!(!result.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_handles_no_result_backend_errors_and_cancellation() {
        let root = tempfile::tempdir().expect("search root");
        let (_program_directory, no_result) = fake_rg("exit 1");
        let result = run(
            &no_result,
            request(root.path(), "none", SearchKind::ByName, 10),
            CancellationToken::new(),
        )
        .await
        .expect("no result is success");
        assert!(result.matches.is_empty());

        let (_program_directory, failure) = fake_rg("echo backend-failed >&2; exit 2");
        assert!(matches!(
            run(
                &failure,
                request(root.path(), "none", SearchKind::ByName, 10),
                CancellationToken::new(),
            )
            .await,
            Err(FileToolError::Io { .. })
        ));

        let (_program_directory, malformed) = fake_rg("echo not-json; while :; do :; done");
        assert!(matches!(
            run(
                &malformed,
                request(root.path(), "none", SearchKind::ByContent, 10),
                CancellationToken::new(),
            )
            .await,
            Err(FileToolError::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_bounds_single_records_and_total_stdout_with_partial_results() {
        let root = tempfile::tempdir().expect("search root");
        let first = root.path().join("alpha.txt");
        let second = root.path().join("alphabet.txt");
        let script = format!(
            "printf '%s\\0%s\\0' {} {}",
            shell_quote(first.to_str().expect("UTF-8 path")),
            shell_quote(second.to_str().expect("UTF-8 path")),
        );
        let (_program_directory, program) = fake_rg(&script);

        let mut oversized = request(root.path(), "alpha", SearchKind::ByName, 10);
        oversized.max_record_bytes = NonZeroU64::new(8).expect("non-zero");
        let result = run(&program, oversized, CancellationToken::new())
            .await
            .expect("oversized record is a truncated success");
        assert!(result.matches.is_empty());
        assert_eq!(
            result.truncation_reason,
            Some(SearchTruncationReason::OversizedRecord)
        );

        let mut total_limited = request(root.path(), "alpha", SearchKind::ByName, 10);
        total_limited.max_output_bytes =
            NonZeroU64::new(first.as_os_str().as_encoded_bytes().len() as u64 + 1)
                .expect("non-zero");
        let result = run(&program, total_limited, CancellationToken::new())
            .await
            .expect("total limit is a truncated success");
        assert_eq!(
            result.matches,
            [SearchMatch::Name {
                path: absolute(&first),
            }]
        );
        assert_eq!(
            result.truncation_reason,
            Some(SearchTruncationReason::MaxOutputBytes)
        );
    }

    #[tokio::test]
    async fn stderr_is_drained_but_only_the_configured_prefix_is_retained() {
        let input = vec![b'x'; 128 * 1024];
        let retained = drain_limited(input.as_slice(), 257)
            .await
            .expect("drain in-memory stderr");
        assert_eq!(retained.len(), 257);
        assert!(retained.iter().all(|byte| *byte == b'x'));
    }

    #[tokio::test]
    async fn search_reports_missing_backend_and_pre_spawn_cancellation() {
        let root = tempfile::tempdir().expect("search root");
        let missing = root.path().join("missing-rg").into_os_string();
        assert!(matches!(
            run(
                &missing,
                request(root.path(), "none", SearchKind::ByName, 10),
                CancellationToken::new(),
            )
            .await,
            Err(FileToolError::SearchBackendUnavailable { .. })
        ));

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            run(
                &missing,
                request(root.path(), "none", SearchKind::ByName, 10),
                cancellation,
            )
            .await,
            Err(FileToolError::Cancelled)
        );
    }

    #[test]
    fn search_content_parser_skips_non_text_payloads_cross_platform() {
        let root = absolute(std::env::temp_dir());
        let binary = serde_json::json!({
            "type": "match",
            "data": {
                "path": {"text": root.as_str()},
                "lines": {"bytes": "AA=="},
                "line_number": 1
            }
        });
        assert_eq!(parse_content_match(&root, &binary), Ok(None));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_cancellation_terminates_a_running_backend() {
        use std::time::Duration;

        let root = tempfile::tempdir().expect("search root");
        let marker = root.path().join("started");
        let script = format!(
            "printf started > {}; while :; do :; done",
            shell_quote(marker.to_str().expect("UTF-8 marker path"))
        );
        let (_program_directory, program) = fake_rg(&script);
        let cancellation = CancellationToken::new();
        let execution_cancellation = cancellation.clone();
        let request = request(root.path(), "none", SearchKind::ByName, 10);
        let execution =
            tokio::spawn(async move { run(&program, request, execution_cancellation).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if tokio::fs::try_exists(&marker).await.expect("check marker") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fake backend must reach its running state");
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), execution)
            .await
            .expect("cancelled backend must settle")
            .expect("search task must not panic");
        assert_eq!(result, Err(FileToolError::Cancelled));
    }

    #[tokio::test]
    #[ignore = "requires system ripgrep on PATH"]
    async fn search_real_ripgrep_in_temporary_directory() {
        let root = tempfile::tempdir().expect("search root");
        tokio::fs::write(root.path().join("visible.txt"), "needle\n")
            .await
            .expect("write fixture");
        tokio::fs::write(root.path().join(".hidden.txt"), "needle\n")
            .await
            .expect("write hidden fixture");
        tokio::fs::write(root.path().join("binary.bin"), b"needle\0binary")
            .await
            .expect("write binary fixture");
        let content = run(
            OsStr::new("rg"),
            request(root.path(), "needle", SearchKind::ByContent, 10),
            CancellationToken::new(),
        )
        .await
        .expect("real ripgrep search");
        assert_eq!(content.matches.len(), 1);
        assert_eq!(
            content.matches[0],
            SearchMatch::Content {
                path: absolute(root.path().join("visible.txt")),
                line_number: NonZeroU32::new(1).expect("non-zero"),
                line: "needle".to_owned(),
            }
        );

        let names = run(
            OsStr::new("rg"),
            request(root.path(), "visible", SearchKind::ByName, 10),
            CancellationToken::new(),
        )
        .await
        .expect("real ripgrep name search");
        assert_eq!(
            names.matches,
            [SearchMatch::Name {
                path: absolute(root.path().join("visible.txt")),
            }]
        );
    }
}
