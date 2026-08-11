use assistant_protocol::{
    ArchiveSessionRequest, AttachmentState, CreateSessionRequest, GetAttachmentRequest,
    IdempotencyKey, ListAttachmentsRequest, SubmitInputRequest,
};

use super::*;
use crate::StagedAttachmentUpload;

#[tokio::test]
async fn attachment_upload_deduplicates_by_session_blob_hash_and_is_read_only_queryable() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first_session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first session")
        .session;
    let second_session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session")
        .session;
    runtime
        .begin_attachment_upload(&first_session.session_id)
        .await
        .expect("upload admission");

    let uploaded = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: first_session.session_id.clone(),
            original_name: "reference.pdf".to_owned(),
            staging_path: "/volatile/upload.part".to_owned(),
            blob_hash: "a".repeat(64),
            size_bytes: 42,
        })
        .await
        .expect("finalize upload")
        .attachment;
    assert_eq!(uploaded.state, AttachmentState::Ready);
    assert_eq!(uploaded.original_name, "reference.pdf");
    assert_eq!(uploaded.size_bytes, 42);
    let duplicate = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: first_session.session_id.clone(),
            original_name: "reference.pdf".to_owned(),
            staging_path: "/volatile/duplicate.part".to_owned(),
            blob_hash: "a".repeat(64),
            size_bytes: 42,
        })
        .await
        .expect("same content upload")
        .attachment;
    assert_eq!(duplicate.attachment_id, uploaded.attachment_id);
    assert_eq!(duplicate.original_name, "reference.pdf");
    assert_eq!(
        runtime
            .get_attachment(GetAttachmentRequest {
                session_id: first_session.session_id.clone(),
                attachment_id: uploaded.attachment_id.clone(),
            })
            .expect("get attachment")
            .attachment,
        uploaded
    );
    assert_eq!(
        runtime
            .list_attachments(ListAttachmentsRequest {
                session_id: first_session.session_id.clone(),
            })
            .expect("list attachments")
            .attachments,
        vec![uploaded.clone()]
    );
    assert!(matches!(
        runtime.get_attachment(GetAttachmentRequest {
            session_id: second_session.session_id,
            attachment_id: uploaded.attachment_id,
        }),
        Err(RuntimeError::AttachmentNotFound { .. })
    ));

    runtime
        .archive_session(ArchiveSessionRequest {
            session_id: first_session.session_id.clone(),
        })
        .await
        .expect("archive session");
    assert!(matches!(
        runtime
            .begin_attachment_upload(&first_session.session_id)
            .await,
        Err(RuntimeError::SessionArchived { .. })
    ));
}

#[tokio::test]
async fn input_freezes_ordered_file_references_and_rejects_invalid_session_relations() {
    let runtime = runtime_with_tools(empty_model(), ToolSetSnapshot::default());
    let first_session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("first session")
        .session;
    let second_session = runtime
        .create_session(CreateSessionRequest::default())
        .await
        .expect("second session")
        .session;
    let first = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: first_session.session_id.clone(),
            original_name: "first.pdf".to_owned(),
            staging_path: "/volatile/first.part".to_owned(),
            blob_hash: "b".repeat(64),
            size_bytes: 10,
        })
        .await
        .expect("first attachment")
        .attachment;
    let second = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: first_session.session_id.clone(),
            original_name: "second.xlsx".to_owned(),
            staging_path: "/volatile/second.part".to_owned(),
            blob_hash: "c".repeat(64),
            size_bytes: 20,
        })
        .await
        .expect("second attachment")
        .attachment;
    let foreign = runtime
        .finalize_attachment_upload(StagedAttachmentUpload {
            session_id: second_session.session_id.clone(),
            original_name: "foreign.txt".to_owned(),
            staging_path: "/volatile/foreign.part".to_owned(),
            blob_hash: "d".repeat(64),
            size_bytes: 30,
        })
        .await
        .expect("foreign attachment")
        .attachment;

    let key = IdempotencyKey::new("files-submit").expect("key");
    let accepted = runtime
        .submit_input(SubmitInputRequest {
            session_id: first_session.session_id.clone(),
            message: "compare".to_owned(),
            attachment_ids: vec![second.attachment_id.clone(), first.attachment_id.clone()],
            idempotency_key: Some(key.clone()),
        })
        .await
        .expect("input with files");
    wait_for_terminal(&runtime, &first_session.session_id, &accepted.run.run_id).await;
    let conversation = runtime
        .conversation_snapshot(&first_session.session_id)
        .await
        .expect("conversation");
    let user = conversation
        .messages
        .iter()
        .find_map(|message| match message {
            ConversationMessage::User(user) => Some(user),
            _ => None,
        })
        .expect("user message");
    assert!(matches!(user.parts[0], UserPart::Text(_)));
    let UserPart::FileReferences(files) = &user.parts[1] else {
        panic!("second user part must contain file references");
    };
    assert_eq!(
        files
            .files
            .iter()
            .map(|file| (file.original_name.as_str(), file.readable_path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                second.original_name.as_str(),
                second.agent_readable_path.as_str()
            ),
            (
                first.original_name.as_str(),
                first.agent_readable_path.as_str()
            ),
        ]
    );

    let reused = runtime
        .submit_input(SubmitInputRequest {
            session_id: first_session.session_id.clone(),
            message: "reuse one file".to_owned(),
            attachment_ids: vec![first.attachment_id.clone()],
            idempotency_key: None,
        })
        .await
        .expect("reuse attachment in another message");
    wait_for_terminal(&runtime, &first_session.session_id, &reused.run.run_id).await;
    let reused_conversation = runtime
        .conversation_snapshot(&first_session.session_id)
        .await
        .expect("conversation with reused attachment");
    let repeated_paths = reused_conversation
        .messages
        .iter()
        .filter_map(|message| match message {
            ConversationMessage::User(user) => Some(&user.parts),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            UserPart::FileReferences(files) => Some(&files.files),
            UserPart::Text(_) | UserPart::Injected(_) => None,
        })
        .flatten()
        .filter(|file| file.original_name == first.original_name)
        .map(|file| file.readable_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        repeated_paths,
        vec![
            first.agent_readable_path.as_str(),
            first.agent_readable_path.as_str()
        ]
    );

    let idempotent = runtime
        .submit_input(SubmitInputRequest {
            session_id: first_session.session_id.clone(),
            message: "ignored retry payload".to_owned(),
            attachment_ids: vec![foreign.attachment_id.clone()],
            idempotency_key: Some(key),
        })
        .await
        .expect("idempotency is checked before attachment resolution");
    assert_eq!(idempotent.input_id, accepted.input_id);
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                session_id: first_session.session_id.clone(),
                message: "duplicate ids".to_owned(),
                attachment_ids: vec![first.attachment_id.clone(), first.attachment_id.clone()],
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::InvalidRequest { .. })
    ));
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                session_id: second_session.session_id,
                message: "cross session".to_owned(),
                attachment_ids: vec![second.attachment_id],
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::AttachmentNotFound { .. })
    ));
    runtime
        .attachments
        .write()
        .expect("attachment registry")
        .get_mut(&first.attachment_id)
        .expect("first attachment")
        .state = crate::StoredAttachmentState::Unavailable;
    assert!(matches!(
        runtime
            .submit_input(SubmitInputRequest {
                session_id: first_session.session_id,
                message: "unavailable".to_owned(),
                attachment_ids: vec![first.attachment_id],
                idempotency_key: None,
            })
            .await,
        Err(RuntimeError::AttachmentUnavailable { .. })
    ));
}
