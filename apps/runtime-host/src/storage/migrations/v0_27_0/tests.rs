use super::*;
use agent_types::{FileReference, FileReferencesPart, TextPart, UserMessage, UserPart};

#[test]
fn relocates_structured_paths_and_queue_references_without_changing_free_text_or_counts() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("home");
    let target = source.join("users/_personal");
    let paths = Relocation {
        source: source.clone(),
        target: target.clone(),
        roots: vec!["data".into()],
    };
    let old = source
        .join("data/sessions/s/attachments/a.txt")
        .to_str()
        .unwrap()
        .to_owned();
    let message = UserMessage {
        id: agent_types::MessageId::new("message").unwrap(),
        origin: Default::default(),
        transcript_visibility: Default::default(),
        parts: vec![
            UserPart::Text(TextPart {
                id: agent_types::PartId::new("text").unwrap(),
                text: old.clone(),
            }),
            UserPart::FileReferences(FileReferencesPart {
                id: agent_types::PartId::new("files").unwrap(),
                files: vec![FileReference {
                    original_name: "a.txt".into(),
                    readable_path: old.clone(),
                }],
            }),
        ],
    };
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE workspaces(user_directory TEXT, agent_directory TEXT, additional_directories_json TEXT); CREATE TABLE session_resources(working_directory TEXT, private_directory TEXT, attachment_directory TEXT, additional_workspace_directories_json TEXT); CREATE TABLE attachments(session_id TEXT, agent_readable_path TEXT, original_name TEXT); CREATE TABLE inputs(input_id TEXT, session_id TEXT, queued_message_json TEXT);").unwrap();
    connection
        .execute("INSERT INTO attachments VALUES ('s',?1,'a.txt')", [&old])
        .unwrap();
    connection
        .execute(
            "INSERT INTO inputs VALUES ('i','s',?1)",
            [serde_json::to_string(&message).unwrap()],
        )
        .unwrap();
    let outside = temporary
        .path()
        .join("external")
        .to_str()
        .unwrap()
        .to_owned();
    connection
        .execute(
            "INSERT INTO workspaces VALUES (?1,?2,?3)",
            rusqlite::params![
                outside,
                source.join("data/workspaces/w/agent").to_str().unwrap(),
                serde_json::to_string(&[old.clone(), outside.clone()]).unwrap()
            ],
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    relocate(&transaction, &paths).unwrap();
    transaction.commit().unwrap();
    let actual: UserMessage = serde_json::from_str(
        &connection
            .query_row("SELECT queued_message_json FROM inputs", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(actual.parts[0], message.parts[0]);
    let UserPart::FileReferences(refs) = &actual.parts[1] else {
        panic!("references")
    };
    assert_eq!(refs.files[0].readable_path, paths.text(&old));
    assert_eq!(
        connection
            .query_row("SELECT user_directory FROM workspaces", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        outside
    );
    for table in ["workspaces", "attachments", "inputs"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(
        paths.text(source.join("database/untouched").to_str().unwrap()),
        source.join("database/untouched").to_str().unwrap()
    );
}
