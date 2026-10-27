use std::{fs, io::Cursor, path::Path};

use agent_types::{TranscriptVisibility, UserMessage, UserMessageOrigin};
use assistant_protocol::{AgentVariant, ConversationOwner, RunId, SessionId};
use assistant_runtime::{StoredHistoryFacts, StoredMessageOrigin, StoredRunHistory};

use super::{codec::*, *};

fn owner() -> ConversationOwner {
    ConversationOwner::MainSession {
        session_id: SessionId::new("file-only-history").unwrap(),
    }
}

#[test]
fn cold_execution_window_discards_old_body_but_keeps_complete_file_and_page_index() {
    let home = home();
    let path = home.path().join("history.jsonl");
    let mut messages = Vec::new();
    for round in 0..8 {
        messages.extend((0..128).map(|index| user(&format!("old-{round}-{index}"), false)));
        messages.push(ConversationMessage::ContextSummary(
            agent_types::ContextSummaryMessage {
                id: MessageId::new(format!("summary-{round}")).unwrap(),
                text: "compact".into(),
                model: None,
                usage: None,
                compacted_usage: None,
                usage_adjustment: None,
                retained_user_message_id: Some(MessageId::new(format!("old-{round}-127")).unwrap()),
                programmatic_context: None,
            },
        ));
    }
    messages.push(user("tail", false));
    let complete = ConversationSnapshot::new(messages);
    super::write_generation(
        &path,
        &owner(),
        1,
        &complete,
        &StoredHistoryFacts::default(),
    )
    .unwrap();
    let bytes_before = fs::read(&path).unwrap();
    let mut indexes = ConversationIndexCache::default();
    let window = indexes
        .read_execution_window(&path, &owner(), 1)
        .unwrap()
        .snapshot();
    assert_eq!(window.messages.len(), 3);
    assert_eq!(message_id(&window.messages[0]).as_str(), "summary-7");
    assert_eq!(message_id(&window.messages[1]).as_str(), "old-7-127");
    assert_eq!(
        indexes.offsets(&path).unwrap().message.len(),
        complete.messages.len()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes_before);
    assert_eq!(window, complete.project_provider_context().unwrap());
}

#[test]
fn streaming_generation_stops_pulling_messages_at_the_first_write_failure() {
    use std::{
        cell::Cell,
        io::{self, Write},
    };

    struct FailingWriter {
        remaining: usize,
    }
    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(io::Error::other("injected write failure"));
            }
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let pulled = Cell::new(0);
    let messages = (0..10_000).map(|index| {
        pulled.set(pulled.get() + 1);
        Ok(user(&format!("lazy-{index}"), false))
    });
    assert!(
        codec::write_generation(
            &mut FailingWriter { remaining: 1_024 },
            &owner(),
            1,
            messages,
            &StoredHistoryFacts::default()
        )
        .is_err()
    );
    assert_eq!(
        pulled.get(),
        64,
        "failed output must not drain or collect the source iterator"
    );
}

#[test]
fn upgrade_streams_large_bodies_and_verifies_every_message_without_a_full_snapshot() {
    use std::{
        fs::File,
        io::{BufWriter, Write},
    };
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    let home = tempfile::Builder::new()
        .prefix("f02-streamed-body-")
        .tempdir_in(base)
        .unwrap();
    let source = home.path().join("legacy.jsonl");
    let target = home.path().join("history.jsonl");
    let message = |index: usize| {
        let ConversationMessage::User(mut message) = user(&format!("large-{index}"), false) else {
            unreachable!()
        };
        message
            .parts
            .push(agent_types::UserPart::Text(agent_types::TextPart {
                id: agent_types::PartId::new(format!("text-{index}")).unwrap(),
                text: format!("message-{index}:{}", "x".repeat(65_536)),
            }));
        ConversationMessage::User(message)
    };
    let mut output = BufWriter::new(File::create(&source).unwrap());
    for index in 0..257 {
        serde_json::to_writer(&mut output, &message(index)).unwrap();
        output.write_all(b"\n").unwrap();
    }
    output.flush().unwrap();
    drop(output);
    let (keys, old_history) = scan_document(&source, &owner(), 1).unwrap();
    assert_eq!(keys.messages.len(), 257);
    assert!(old_history.is_none());
    write_upgraded_generation(
        &source,
        &target,
        &owner(),
        1,
        2,
        257,
        &StoredHistoryFacts::default(),
    )
    .unwrap();
    let mut reader = RecordReader::new(std::io::BufReader::new(File::open(&target).unwrap()));
    let mut total = 0;
    while let Some(batch) = reader.next().unwrap() {
        assert!(batch.messages.len() <= 64);
        total += batch.messages.len();
    }
    assert_eq!(total, 257);
    assert_eq!(
        stream::verify_generation(
            &target,
            &owner(),
            2,
            (0..257).map(|index| Ok(message(index))),
            &StoredHistoryFacts::default()
        )
        .unwrap(),
        257
    );
    // 同 ID、同数量但中间正文被替换，必须拒绝，不能只比较主键和首尾。
    assert!(
        stream::verify_generation(
            &target,
            &owner(),
            2,
            (0..257).map(|index| {
                let mut expected = message(index);
                if index == 128
                    && let ConversationMessage::User(user) = &mut expected
                    && let agent_types::UserPart::Text(text) = &mut user.parts[0]
                {
                    text.text.push('!');
                }
                Ok(expected)
            }),
            &StoredHistoryFacts::default()
        )
        .is_err()
    );
}

fn user(id: &str, hidden: bool) -> ConversationMessage {
    ConversationMessage::User(UserMessage {
        id: MessageId::new(id).unwrap(),
        origin: if hidden {
            UserMessageOrigin::Runtime
        } else {
            UserMessageOrigin::User
        },
        transcript_visibility: if hidden {
            TranscriptVisibility::Hidden
        } else {
            TranscriptVisibility::Visible
        },
        parts: Vec::new(),
    })
}

fn run() -> StoredRunHistory {
    StoredRunHistory {
        finished_after_message_id: None,
        run_id: RunId::new("run-one").unwrap(),
        input_id: None,
        user_message_id: None,
        attempt: 1,
        agent_variant: AgentVariant::Build,
        approval_mode: assistant_protocol::ApprovalMode::Ask,
        reasoning_effort: None,
        agent_shell_kind: None,
        status: Some(assistant_protocol::RunStatus::Running),
        error: None,
        cancel_requested: false,
        created_at_ms: 10,
        started_at_ms: Some(20),
        finished_at_ms: None,
        inherited: false,
        partial: false,
        last_message_id: None,
    }
}

fn header() -> HistoryRecord {
    HistoryRecord::Header(HistoryHeader {
        format: HISTORY_FORMAT.to_owned(),
        version: HISTORY_VERSION,
        owner: owner(),
        generation: 1,
    })
}

fn commit(
    id: &str,
    messages: Vec<ConversationMessage>,
    history: StoredHistoryFacts,
) -> HistoryRecord {
    HistoryRecord::Commit(HistoryFrame {
        operation_id: id.to_owned(),
        messages,
        history,
    })
}

fn bytes(records: &[HistoryRecord]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn home() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
    fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("f02-history-codec-")
        .tempdir_in(base)
        .unwrap()
}

#[test]
fn metadata_only_start_is_reliable_without_an_invented_message() {
    let expected = StoredHistoryFacts {
        runs: vec![run()],
        ..Default::default()
    };
    let encoded = bytes(&[header(), commit("start", Vec::new(), expected.clone())]);
    let (conversation, facts, header) = decode_history(Cursor::new(encoded)).unwrap();
    assert!(conversation.messages.is_empty());
    assert_eq!(facts, expected);
    assert_eq!(header.unwrap().owner, owner());
}

#[test]
fn batched_frames_preserve_raw_and_visible_ordinals_without_counting_metadata() {
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    let messages = vec![
        user("first", false),
        user("internal", true),
        user("second", false),
    ];
    let encoded = bytes(&[
        header(),
        commit(
            "start",
            Vec::new(),
            StoredHistoryFacts {
                runs: vec![run()],
                ..Default::default()
            },
        ),
        commit("messages", messages.clone(), StoredHistoryFacts::default()),
    ]);
    fs::write(&path, encoded).unwrap();
    let mut cache = ConversationIndexCache::default();
    let first = cache.read_window(&path, 1, Some(1), 1).unwrap();
    assert_eq!((first.start, first.end, first.total), (0, 1, 2));
    assert_eq!(first.conversation.messages, messages[..2]);
    let last = cache.read_window(&path, 1, None, 1).unwrap();
    assert_eq!(last.conversation.messages, messages[2..]);
    let (raw, end, total) = cache.read_raw_window(&path, 1, 1).unwrap();
    assert_eq!((end, total), (2, 3));
    assert_eq!(raw.messages, messages[1..2]);
    assert_eq!(
        cache
            .locate_message(&path, &MessageId::new("second").unwrap())
            .unwrap(),
        Some((2, Some(1)))
    );
    assert_eq!(
        cache
            .locate_message(&path, &MessageId::new("internal").unwrap())
            .unwrap(),
        Some((1, None))
    );
}

#[test]
fn cache_rebuilds_after_a_new_commit_and_rejects_a_broken_tail() {
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    let encoded = bytes(&[
        header(),
        commit("first", vec![user("one", false)], Default::default()),
    ]);
    fs::write(&path, &encoded).unwrap();
    let mut cache = ConversationIndexCache::default();
    assert_eq!(cache.read_window(&path, 1, None, 10).unwrap().total, 1);
    let appended = bytes(&[commit("next", vec![user("two", false)], Default::default())]);
    reconcile_append(&path, encoded.len() as u64, &appended).unwrap();
    assert_eq!(cache.read_window(&path, 1, None, 10).unwrap().total, 2);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":")
        .unwrap();
    assert!(cache.read_window(&path, 1, None, 10).is_err());
}

#[test]
fn empty_framed_file_has_zero_messages_and_a_real_header() {
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    fs::write(&path, bytes(&[header()])).unwrap();
    assert!(read_owned(&path, &owner(), 1).unwrap().messages.is_empty());
    let mut cache = ConversationIndexCache::default();
    assert_eq!(cache.read_window(&path, 1, None, 10).unwrap().total, 0);
    assert_eq!(
        cache
            .locate_message(&path, &MessageId::new("missing").unwrap())
            .unwrap(),
        None
    );
}

#[test]
fn legacy_migration_source_remains_readable_but_never_mixes_with_frames() {
    let legacy = encode_messages(&[user("source", false)]).unwrap();
    assert_eq!(
        decode(Cursor::new(&legacy)).unwrap().messages,
        vec![user("source", false)]
    );
    let framed = bytes(&[
        header(),
        commit("new", vec![user("new", false)], Default::default()),
    ]);
    assert!(decode(Cursor::new([legacy.clone(), framed.clone()].concat())).is_err());
    assert!(decode(Cursor::new([framed, legacy].concat())).is_err());
}

#[test]
fn missing_duplicate_and_unknown_headers_fail_closed() {
    let no_header = bytes(&[commit("new", vec![user("new", false)], Default::default())]);
    assert!(decode(Cursor::new(no_header)).is_err());
    assert!(decode(Cursor::new(bytes(&[header(), header()]))).is_err());
    let mut unknown: serde_json::Value = serde_json::to_value(header()).unwrap();
    unknown["version"] = 2.into();
    let mut encoded = serde_json::to_vec(&unknown).unwrap();
    encoded.push(b'\n');
    assert!(decode(Cursor::new(encoded)).is_err());
}

#[test]
fn empty_commits_and_partial_history_with_future_terminal_facts_are_rejected() {
    let empty = bytes(&[header(), commit("empty", Vec::new(), Default::default())]);
    assert!(decode(Cursor::new(empty)).is_err());
    let mut partial = run();
    partial.partial = true;
    partial.status = Some(assistant_protocol::RunStatus::Completed);
    partial.finished_at_ms = Some(90);
    let encoded = bytes(&[
        header(),
        commit(
            "fork",
            vec![user("prefix", false)],
            StoredHistoryFacts {
                runs: vec![partial],
                ..Default::default()
            },
        ),
    ]);
    assert!(decode(Cursor::new(encoded)).is_err());
}

#[test]
fn duplicate_ids_and_dangling_message_origins_are_not_silently_repaired() {
    let duplicate = bytes(&[
        header(),
        commit(
            "duplicate",
            vec![user("same", false), user("same", false)],
            Default::default(),
        ),
    ]);
    assert!(decode(Cursor::new(duplicate)).is_err());
    let facts = StoredHistoryFacts {
        message_origins: vec![StoredMessageOrigin {
            message_id: MessageId::new("missing").unwrap(),
            run_id: None,
            input_id: None,
            attempt: None,
            step: None,
            inherited: true,
        }],
        ..Default::default()
    };
    assert!(
        decode(Cursor::new(bytes(&[
            header(),
            commit("orphan", Vec::new(), facts)
        ])))
        .is_err()
    );
}

#[test]
fn incomplete_header_or_frame_is_not_an_empty_or_successful_history() {
    for encoded in [
        bytes(&[header()]),
        bytes(&[
            header(),
            commit("one", vec![user("one", false)], Default::default()),
        ]),
    ] {
        assert!(decode(Cursor::new(&encoded[..encoded.len() - 1])).is_err());
    }
}

#[test]
fn generation_batches_and_old_pages_read_latest_metadata_in_file_order() {
    use agent_types::{AssistantMessage, FinishReason, ModelIdentity, ProviderId};
    use assistant_runtime::StoredHistoryFeedback;
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    let first = ConversationMessage::Assistant(AssistantMessage {
        id: MessageId::new("early-assistant").unwrap(),
        purpose: Default::default(),
        model: ModelIdentity::new(ProviderId::new("fixture").unwrap(), "fixture"),
        parts: Vec::new(),
        finish_reason: FinishReason::Stop,
        usage: None,
    });
    let mut messages = vec![first];
    messages.extend((0..129).map(|index| user(&format!("user-{index}"), false)));
    let history = StoredHistoryFacts {
        runs: vec![run()],
        message_origins: messages
            .iter()
            .map(|message| StoredMessageOrigin {
                message_id: message_id(message).clone(),
                run_id: Some(RunId::new("run-one").unwrap()),
                input_id: None,
                attempt: Some(1),
                step: Some(1),
                inherited: false,
            })
            .collect(),
        ..Default::default()
    };
    let snapshot = ConversationSnapshot::new(messages);
    super::write_generation(&path, &owner(), 1, &snapshot, &history).unwrap();
    let encoded = fs::read_to_string(&path).unwrap();
    for line in encoded.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        if let Some(messages) = value.get("messages").and_then(serde_json::Value::as_array) {
            assert!(messages.len() <= 64);
        }
    }
    let feedback = StoredHistoryFeedback {
        message_id: assistant_protocol::MessageId::new("early-assistant").unwrap(),
        feedback: Some(assistant_protocol::MessageFeedback::Positive),
        changed_at_ms: 100,
    };
    let mut terminal = run();
    terminal.status = Some(assistant_protocol::RunStatus::Completed);
    terminal.finished_at_ms = Some(100);
    let last_run = encode_history_append(
        "terminal",
        &[],
        StoredHistoryFacts {
            runs: vec![terminal],
            feedback: vec![feedback.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    reconcile_append(&path, path.metadata().unwrap().len(), &last_run).unwrap();
    let clear = encode_history_append(
        "clear-feedback",
        &[],
        StoredHistoryFacts {
            feedback: vec![StoredHistoryFeedback {
                feedback: None,
                changed_at_ms: 200,
                ..feedback
            }],
            ..Default::default()
        },
    )
    .unwrap();
    reconcile_append(&path, path.metadata().unwrap().len(), &clear).unwrap();
    let mut cache = ConversationIndexCache::default();
    let page = cache.read_window(&path, 1, Some(1), 1).unwrap();
    assert_eq!(page.conversation.messages, snapshot.messages[..1]);
    let facts = page.history.unwrap();
    assert_eq!(facts.message_origins.len(), 1);
    assert_eq!(facts.runs.len(), 1);
    assert_eq!(
        facts.runs[0].status,
        Some(assistant_protocol::RunStatus::Completed)
    );
    assert_eq!(facts.runs[0].finished_at_ms, Some(100));
    assert_eq!(facts.feedback.len(), 1);
    assert_eq!(facts.feedback[0].feedback, None);
    assert_eq!(facts.feedback[0].changed_at_ms, 200);
}

#[test]
fn foreign_owner_metadata_is_rejected_before_staged_effect_or_page_projection() {
    let facts = StoredHistoryFacts {
        mcp_selections: vec![assistant_runtime::StoredMcpSelection {
            selection_id: "foreign-selection".into(),
            session_id: SessionId::new("another-session").unwrap(),
            input_id: None,
            message_id: MessageId::new("source").unwrap(),
            server_key: assistant_protocol::McpServerKey::new("fixture").unwrap(),
            display_name: "Fixture".into(),
            created_at_ms: 10,
        }],
        ..Default::default()
    };
    let payload = encode_history_append("foreign", &[user("source", false)], facts).unwrap();
    assert!(decode_append(&payload, true, &owner()).is_err());
    let encoded = [bytes(&[header()]), payload].concat();
    assert!(decode(Cursor::new(&encoded)).is_err());
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    fs::write(&path, encoded).unwrap();
    assert!(
        ConversationIndexCache::default()
            .read_window(&path, 1, None, 10)
            .is_err()
    );
}

#[test]
fn file_header_identity_is_checked_even_when_history_ids_repeat() {
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    fs::write(
        &path,
        bytes(&[
            header(),
            commit("one", vec![user("same-id", false)], Default::default()),
        ]),
    )
    .unwrap();
    let other_owner = ConversationOwner::MainSession {
        session_id: SessionId::new("another-session").unwrap(),
    };
    let mut cache = ConversationIndexCache::default();
    cache.validate_owner(&path, &owner(), 1).unwrap();
    assert!(cache.validate_owner(&path, &other_owner, 1).is_err());
    assert!(cache.validate_owner(&path, &owner(), 2).is_err());
    assert!(cache.read_window(&path, 2, None, 10).is_err());
    assert!(read_owned(&path, &other_owner, 1).is_err());
    assert!(read_owned(&path, &owner(), 2).is_err());
    assert_eq!(
        read_owned(&path, &owner(), 1).unwrap().messages,
        vec![user("same-id", false)]
    );
}

#[test]
fn a_tool_exchange_is_atomic_even_if_two_adjacent_frames_would_pair() {
    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, ModelIdentity, ProviderId, ToolCall,
        ToolCallId, ToolMessage, ToolName, ToolResult, ToolResultContent, ToolResultStatus,
    };
    let call_id = ToolCallId::new("call-one").unwrap();
    let assistant = ConversationMessage::Assistant(AssistantMessage {
        id: MessageId::new("assistant-one").unwrap(),
        purpose: Default::default(),
        model: ModelIdentity::new(ProviderId::new("fixture").unwrap(), "fixture"),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: call_id.clone(),
            name: ToolName::new("read_file").unwrap(),
            arguments: serde_json::json!({}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    });
    let result = ConversationMessage::Tool(ToolMessage {
        id: MessageId::new("tool-one").unwrap(),
        result: ToolResult {
            call_id,
            status: ToolResultStatus::Success,
            content: ToolResultContent::text("done"),
            metadata: None,
        },
    });
    let broken = bytes(&[
        header(),
        commit("begin", vec![assistant.clone()], Default::default()),
        commit("result", vec![result.clone()], Default::default()),
    ]);
    assert!(decode(Cursor::new(broken)).is_err());
    let closed = vec![assistant, result];
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    fs::write(
        &path,
        bytes(&[
            header(),
            commit("closed", closed.clone(), Default::default()),
        ]),
    )
    .unwrap();
    assert_eq!(read_owned(&path, &owner(), 1).unwrap().messages, closed);
    let mut cache = ConversationIndexCache::default();
    let page = cache.read_window(&path, 1, None, 1).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.conversation.messages, closed);

    // 批次上限恰好落在调用与结果之间时，整个交换随同一物理帧写入。
    let mut prefix: Vec<_> = (0..63)
        .map(|index| user(&format!("prefix-{index}"), false))
        .collect();
    prefix.extend(closed.clone());
    let target = home.path().join("conversation.2.jsonl");
    super::write_generation(
        &target,
        &owner(),
        2,
        &ConversationSnapshot::new(prefix.clone()),
        &StoredHistoryFacts::default(),
    )
    .unwrap();
    let mut records = RecordReader::new(std::io::BufReader::new(
        std::fs::File::open(target).unwrap(),
    ));
    assert_eq!(records.next().unwrap().unwrap().messages, prefix);
    assert!(records.next().unwrap().is_none());

    // 每帧各自配对仍不足够：同一压缩作用域不能跨帧复用调用 ID。
    let mut repeated = closed.clone();
    if let ConversationMessage::Assistant(assistant) = &mut repeated[0] {
        assistant.id = MessageId::new("assistant-two").unwrap();
    }
    if let ConversationMessage::Tool(tool) = &mut repeated[1] {
        tool.id = MessageId::new("tool-two").unwrap();
    }
    // 候选追加也必须沿用整个可靠前缀的调用 ID 作用域；失败不污染后续校验。
    assert!(
        cache
            .validate_append(&path, &owner(), 1, &repeated)
            .is_err()
    );
    assert!(
        cache
            .validate_append(&path, &owner(), 1, &[closed[1].clone()])
            .is_err()
    );
    assert!(
        cache
            .validate_append(&path, &owner(), 1, &[user("new-user", false)])
            .is_ok()
    );
    fs::write(
        &path,
        bytes(&[
            header(),
            commit("first-closed", closed, Default::default()),
            commit("repeated-call", repeated, Default::default()),
        ]),
    )
    .unwrap();
    assert!(scan_document(&path, &owner(), 1).is_err());
    assert!(cache.read_window(&path, 1, None, 10).is_err());
}

#[test]
fn committed_append_index_matches_a_full_rebuild_and_rejects_duplicate_ids() {
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    fs::write(
        &path,
        bytes(&[
            header(),
            commit("first", vec![user("first", false)], Default::default()),
        ]),
    )
    .unwrap();
    let mut cache = ConversationIndexCache::default();
    for (operation, messages, facts) in [
        (
            "second",
            vec![user("second", false), user("hidden", true)],
            StoredHistoryFacts::default(),
        ),
        (
            "feedback",
            vec![],
            StoredHistoryFacts {
                feedback: vec![assistant_runtime::StoredHistoryFeedback {
                    message_id: assistant_protocol::MessageId::new("first").unwrap(),
                    feedback: None,
                    changed_at_ms: 20,
                }],
                ..Default::default()
            },
        ),
    ] {
        cache
            .validate_append(&path, &owner(), 1, &messages)
            .unwrap();
        let base = path.metadata().unwrap().len();
        let payload = encode_history_append(operation, &messages, facts).unwrap();
        let batch = decode_append(&payload, true, &owner()).unwrap();
        cache.retain_current_prefix(&path);
        reconcile_append(&path, base, &payload).unwrap();
        cache.record_committed_append(&path, base, payload.len(), &owner(), 1, &batch);
        let mut rebuilt = ConversationIndexCache::default();
        assert_eq!(
            cache.read_window(&path, 1, None, 50).unwrap(),
            rebuilt.read_window(&path, 1, None, 50).unwrap()
        );
        assert_eq!(
            cache.read_history(&path, &owner(), 1).unwrap(),
            rebuilt.read_history(&path, &owner(), 1).unwrap()
        );
    }
    assert!(
        cache
            .validate_append(&path, &owner(), 1, &[user("second", false)])
            .is_err()
    );
    assert!(cache.validate_append(&path, &owner(), 2, &[]).is_err());

    // 同长度损坏也必须失效；不能只按字节数复用已经校验过的索引。
    let mut damaged = fs::read(&path).unwrap();
    damaged[0] = b'!';
    fs::write(&path, damaged).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
    assert!(cache.validate_append(&path, &owner(), 1, &[]).is_err());
}

#[test]
fn child_directory_lookup_selects_latest_target_and_preserves_owner_checks() {
    use assistant_protocol::{ChildTaskId, ChildTaskStatus};
    use assistant_runtime::StoredChildTaskHistory;
    let child = |id: &str| StoredChildTaskHistory {
        child_task_id: ChildTaskId::new(id).unwrap(),
        parent_run_id: RunId::new("parent-run").unwrap(),
        parent_tool_call_id: assistant_protocol::ToolCallId::new("parent-call").unwrap(),
        title: id.into(),
        agent_variant: AgentVariant::Build,
        context_management_mode: Default::default(),
        context_window_tokens: None,
        status: Some(ChildTaskStatus::Completed),
        body_generation: 1,
        message_count: 0,
        final_message_id: None,
        error: None,
        cancel_requested: false,
        created_at_ms: 1,
        started_at_ms: Some(1),
        finished_at_ms: Some(2),
        inherited: false,
        body_unavailable: false,
    };
    let first = child("first");
    let second = child("second");
    let mut latest = first.clone();
    latest.body_generation = 2;
    latest.title = "updated".into();
    let home = home();
    let path = home.path().join("conversation.1.jsonl");
    fs::write(
        &path,
        bytes(&[
            header(),
            commit(
                "initial",
                Vec::new(),
                StoredHistoryFacts {
                    child_tasks: vec![first, second.clone()],
                    ..Default::default()
                },
            ),
            commit(
                "updated",
                Vec::new(),
                StoredHistoryFacts {
                    child_tasks: vec![latest.clone()],
                    ..Default::default()
                },
            ),
        ]),
    )
    .unwrap();
    let mut cache = ConversationIndexCache::default();
    assert_eq!(
        cache
            .read_child_history(&path, &owner(), 1, Some(&latest.child_task_id))
            .unwrap(),
        vec![latest.clone()]
    );
    assert_eq!(
        cache
            .read_child_history(&path, &owner(), 1, Some(&second.child_task_id))
            .unwrap(),
        vec![second.clone()]
    );
    assert_eq!(
        cache.read_child_history(&path, &owner(), 1, None).unwrap(),
        vec![latest, second]
    );
    assert!(
        cache
            .read_child_history(
                &path,
                &owner(),
                1,
                Some(&ChildTaskId::new("missing").unwrap())
            )
            .unwrap()
            .is_empty()
    );
    assert!(cache.read_child_history(&path, &owner(), 2, None).is_err());
    let foreign = ConversationOwner::MainSession {
        session_id: SessionId::new("foreign").unwrap(),
    };
    assert!(cache.read_child_history(&path, &foreign, 1, None).is_err());
}
