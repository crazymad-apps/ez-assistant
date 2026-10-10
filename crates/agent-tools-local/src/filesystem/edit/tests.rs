use super::super::tests::{absolute, fixture};
use super::*;
use agent_tools::{EditReplacement, FileSystemTool};

fn entry(path: &std::path::Path, pairs: &[(&str, &str)]) -> EditFileEntry {
    EditFileEntry {
        path: absolute(path),
        edits: pairs
            .iter()
            .map(|(old, new)| EditReplacement {
                old_string: (*old).to_owned(),
                new_string: (*new).to_owned(),
                replace_all: false,
            })
            .collect(),
    }
}

#[tokio::test]
async fn ordered_edits_and_first_failure_preserve_actual_commits() {
    let (root, fs) = fixture(1024);
    let paths = ["a", "b", "c"].map(|name| root.path().join(name));
    for path in &paths {
        std::fs::write(path, "old").unwrap();
    }
    let request = EditFileRequest {
        files: vec![
            entry(&paths[0], &[("old", "mid"), ("mid", "new")]),
            entry(&paths[1], &[("old", "mid"), ("missing", "new")]),
            entry(&paths[2], &[("old", "new")]),
        ],
    };
    let result = fs.edit(request, FileToolContext::default()).await.unwrap();
    assert_eq!(
        result.files[0].outcome,
        EditFileOutcome::Success { replacements: 2 }
    );
    assert!(matches!(
        result.files[1].outcome,
        EditFileOutcome::Failed {
            edit_index: Some(1),
            ..
        }
    ));
    assert_eq!(
        result.files[2].outcome,
        EditFileOutcome::NotExecuted {
            reason: EditNotExecutedReason::PreviousFailure
        }
    );
    assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "new");
    for path in &paths[1..] {
        assert_eq!(std::fs::read_to_string(path).unwrap(), "old");
    }
}

#[tokio::test]
async fn missing_target_and_duplicate_aliases_fail_before_any_write() {
    let (root, fs) = fixture(1024);
    let path = root.path().join("a");
    std::fs::write(&path, "old").unwrap();
    let result = fs
        .edit(
            EditFileRequest {
                files: vec![
                    entry(&path, &[("old", "new")]),
                    entry(&root.path().join("missing"), &[("old", "new")]),
                ],
            },
            FileToolContext::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        result.files[0].outcome,
        EditFileOutcome::NotExecuted {
            reason: EditNotExecutedReason::PreflightFailed
        }
    ));
    assert!(matches!(
        result.files[1].outcome,
        EditFileOutcome::Failed { .. }
    ));
    let mut aliases = vec![path.clone()];
    let hard = root.path().join("hard");
    std::fs::hard_link(&path, &hard).unwrap();
    aliases.push(hard);
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        aliases.push(link);
    }
    for alias in aliases {
        let result = fs
            .edit(
                EditFileRequest {
                    files: vec![
                        entry(&path, &[("old", "new")]),
                        entry(&alias, &[("old", "new")]),
                    ],
                },
                FileToolContext::default(),
            )
            .await
            .unwrap();
        assert!(
            matches!(&result.files[1].outcome, EditFileOutcome::Failed { code, .. } if code == "duplicate_target")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }
}

#[tokio::test]
async fn successful_link_edit_preserves_links_and_limits_expansion_before_commit() {
    let (root, fs) = fixture(32);
    let path = root.path().join("a");
    let hard = root.path().join("hard");
    std::fs::write(&path, b"\xEF\xBB\xBFold\r\nline\r\n").unwrap();
    std::fs::hard_link(&path, &hard).unwrap();
    let result = fs
        .edit(
            EditFileRequest {
                files: vec![entry(&hard, &[("old\nline", "mid\nline"), ("mid", "new")])],
            },
            FileToolContext::default(),
        )
        .await
        .unwrap();
    assert!(matches!(
        result.files[0].outcome,
        EditFileOutcome::Success { replacements: 2 }
    ));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"\xEF\xBB\xBFnew\r\nline\r\n"
    );
    let before = std::fs::read(&path).unwrap();
    let result = fs
        .edit(
            EditFileRequest {
                files: vec![entry(&path, &[("new", &"x".repeat(40))])],
            },
            FileToolContext::default(),
        )
        .await
        .unwrap();
    assert!(
        matches!(&result.files[0].outcome, EditFileOutcome::Failed { code, .. } if code == "too_large")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn cancellation_while_waiting_for_second_lock_keeps_first_success() {
    let (root, fs) = fixture(1024);
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::write(&a, "old").unwrap();
    std::fs::write(&b, "old").unwrap();
    let lock = fs.locks.lock_for(&absolute(&b));
    let guard = lock.lock().await;
    let token = tokio_util::sync::CancellationToken::new();
    let request = EditFileRequest {
        files: vec![entry(&a, &[("old", "new")]), entry(&b, &[("old", "new")])],
    };
    let edit = fs.edit(request, FileToolContext::new(token.clone()));
    let cancel = async {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while tokio::fs::read(&a).await.unwrap() != b"new" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        token.cancel();
    };
    let (result, ()) = tokio::join!(edit, cancel);
    drop(guard);
    let result = result.unwrap();
    assert_eq!(
        result.files[0].outcome,
        EditFileOutcome::Success { replacements: 1 }
    );
    assert_eq!(
        result.files[1].outcome,
        EditFileOutcome::NotExecuted {
            reason: EditNotExecutedReason::Cancelled
        }
    );
    assert_eq!(std::fs::read_to_string(b).unwrap(), "old");
}

#[test]
fn replaced_path_and_failed_truncation_cannot_report_success() {
    let (root, _) = fixture(1024);
    let path = root.path().join("a");
    std::fs::write(&path, "old").unwrap();
    let expected = identity(&absolute(&path)).unwrap();
    std::fs::rename(&path, root.path().join("old-object")).unwrap();
    std::fs::write(&path, "old").unwrap();
    assert!(matches!(
        edit_one(
            &entry(&path, &[("old", "new")]),
            &expected,
            1024.try_into().unwrap(),
            &FileToolContext::default()
        ),
        EditFileOutcome::Failed { .. }
    ));
    let mut readonly = File::open(&path).unwrap();
    assert!(matches!(
        commit(&mut readonly, b"new", 1),
        EditFileOutcome::Unknown { .. }
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "old");
}

#[test]
fn partial_write_and_flush_errors_are_unknown_without_retry() {
    struct BrokenWriter {
        bytes: Vec<u8>,
        fail_write: bool,
        calls: usize,
    }
    impl Write for BrokenWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.fail_write && self.calls > 1 {
                return Err(std::io::Error::other("injected write failure"));
            }
            let count = if self.fail_write { 1 } else { bytes.len() };
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("injected flush failure"))
        }
    }
    for fail_write in [true, false] {
        let mut writer = BrokenWriter {
            bytes: vec![],
            fail_write,
            calls: 0,
        };
        let outcome = commit_after_truncate(Ok(()), &mut writer, b"new", 1);
        assert!(matches!(outcome, EditFileOutcome::Unknown { .. }));
        assert_eq!(
            writer.bytes,
            if fail_write {
                b"n".as_slice()
            } else {
                b"new".as_slice()
            }
        );
        assert_eq!(writer.calls, if fail_write { 2 } else { 1 });
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_case_alias_is_rejected_before_any_write() {
    let (root, fs) = fixture(1024);
    let path = root.path().join("case-alias.txt");
    std::fs::write(&path, "old").unwrap();
    let alias = root.path().join("CASE-ALIAS.TXT");
    // Windows 可启用目录大小写敏感；此用例只适用于实际同一对象的默认目录。
    if !alias.exists() {
        return;
    }
    let result = fs
        .edit(
            EditFileRequest {
                files: vec![
                    entry(&path, &[("old", "new")]),
                    entry(&alias, &[("old", "new")]),
                ],
            },
            FileToolContext::default(),
        )
        .await
        .unwrap();
    assert!(
        matches!(&result.files[1].outcome,EditFileOutcome::Failed {code,..} if code == "duplicate_target")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "old");
}
