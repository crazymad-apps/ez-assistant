//! Attachment Blob、Session 稳定视图与 SQLite 元数据的一致提交和启动修复。

use std::{fs, path::Path};

use assistant_protocol::{AttachmentId, SessionId};
use assistant_runtime::{
    NewAttachmentUpload, StoreError, StoreErrorKind, StoredAttachment, StoredAttachmentState,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    StorageEngine, StorageResult, attachment_io, database_write_error, internal_error,
    invalid_data, invalid_data_with_source, non_negative_u64, to_i64,
};

impl StorageEngine {
    pub(super) fn upload_attachment(
        &mut self,
        upload: NewAttachmentUpload,
    ) -> StorageResult<StoredAttachment> {
        super::filesystem::validate_attachment_component(&upload.attachment_id)?;
        super::filesystem::validate_session_component(&upload.session_id)?;
        attachment_io::validate_original_name(&upload.original_name)?;
        attachment_io::validate_blob_hash(&upload.blob_hash)?;
        let staging_path = Path::new(&upload.staging_path);
        attachment_io::validate_staging_path(
            &self.upload_staging_directory,
            staging_path,
            upload.size_bytes,
        )?;
        attachment_io::validate_staging_hash(
            staging_path,
            &upload.original_name,
            &upload.blob_hash,
        )?;

        if let Some(existing) =
            self.attachment_by_blob_hash(&upload.session_id, &upload.blob_hash)?
        {
            fs::remove_file(staging_path).map_err(|source| {
                internal_error(
                    "duplicate attachment staging file could not be removed",
                    source,
                )
            })?;
            return Ok(existing);
        }

        let (lifecycle, attachment_directory) = self
            .connection
            .query_row(
                "SELECT s.lifecycle, r.attachment_directory
                 FROM sessions s
                 JOIN session_resources r ON r.session_id = s.session_id
                 WHERE s.session_id = ?1",
                [upload.session_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|source| internal_error("attachment session could not be queried", source))?
            .ok_or_else(|| {
                StoreError::new(
                    StoreErrorKind::Conflict,
                    "attachment session does not exist",
                )
            })?;
        if lifecycle != "active" {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "attachment session is archived",
            ));
        }

        let data_directory = self
            .blobs_directory
            .parent()
            .expect("blobs directory is inside data directory");
        let expected_blob_relative =
            attachment_io::blob_relative_path(&upload.blob_hash, &upload.original_name);
        if let Some((stored_size, stored_relative, stored_media_type)) = self
            .connection
            .query_row(
                "SELECT size_bytes, relative_path, media_type FROM attachment_blobs WHERE blob_hash = ?1",
                [upload.blob_hash.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|source| {
                internal_error("attachment blob metadata could not be queried", source)
            })?
            && (non_negative_u64(stored_size, "stored attachment size is invalid")?
                != upload.size_bytes
                || Path::new(&stored_relative) != expected_blob_relative
                || (stored_media_type.is_some() && stored_media_type != upload.media_type))
        {
            return Err(invalid_data(
                "stored attachment blob metadata is inconsistent",
            ));
        }
        let (blob_relative_path, _) = attachment_io::ensure_blob(
            data_directory,
            staging_path,
            &upload.original_name,
            &upload.blob_hash,
            upload.size_bytes,
        )?;
        let view = attachment_io::stable_view_path(
            Path::new(&attachment_directory),
            &upload.attachment_id,
            &upload.original_name,
        );
        let created_view =
            attachment_io::ensure_stable_view(&view, &upload.blob_hash, &upload.original_name)?;
        let agent_readable_path = view
            .to_str()
            .ok_or_else(|| {
                StoreError::new(
                    StoreErrorKind::InvalidInput,
                    "attachment path must be UTF-8",
                )
            })?
            .to_owned();

        let persisted = (|| -> StorageResult<()> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|source| {
                    database_write_error("attachment transaction could not be started", source)
                })?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO attachment_blobs (
                        blob_hash, size_bytes, relative_path, media_type, created_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        upload.blob_hash,
                        to_i64(upload.size_bytes, "attachment size is too large")?,
                        path_text(&blob_relative_path)?,
                        upload.media_type,
                        upload.created_at_ms,
                    ],
                )
                .map_err(|source| {
                    database_write_error("attachment blob metadata could not be created", source)
                })?;
            transaction
                .execute(
                    "INSERT INTO attachments (
                        attachment_id, session_id, blob_hash, original_name,
                        agent_readable_path, state, created_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 'ready', ?6)",
                    params![
                        upload.attachment_id.as_str(),
                        upload.session_id.as_str(),
                        upload.blob_hash,
                        upload.original_name,
                        agent_readable_path,
                        upload.created_at_ms,
                    ],
                )
                .map_err(|source| {
                    database_write_error("attachment metadata could not be created", source)
                })?;
            transaction.commit().map_err(|source| {
                database_write_error("attachment transaction could not be committed", source)
            })
        })();
        if let Err(error) = persisted {
            if created_view {
                let _ = fs::remove_file(&view);
                if let Some(parent) = view.parent() {
                    let _ = fs::remove_dir(parent);
                }
            }
            // 另一条并发上传可能已经提交同一 Blob；仍在返回错误前做一次只读核对，
            // 避免把已经形成的同 Session 附件事实误报为失败。
            if let Some(existing) =
                self.attachment_by_blob_hash(&upload.session_id, &upload.blob_hash)?
            {
                return Ok(existing);
            }
            return Err(error);
        }
        self.attachment_by_id(&upload.attachment_id)?
            .ok_or_else(|| invalid_data("committed attachment could not be loaded"))
    }

    pub(super) fn recover_attachments(&mut self) -> StorageResult<()> {
        let rows = self.load_attachments()?;
        for attachment in rows {
            let state = self.validate_or_repair_attachment(&attachment);
            let state = if state.is_ok() {
                "ready"
            } else {
                "unavailable"
            };
            self.connection
                .execute(
                    "UPDATE attachments SET state = ?2 WHERE attachment_id = ?1",
                    params![attachment.attachment_id.as_str(), state],
                )
                .map_err(|source| {
                    database_write_error("attachment state could not be repaired", source)
                })?;
        }
        Ok(())
    }

    pub(super) fn load_attachments(&self) -> StorageResult<Vec<StoredAttachment>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT a.attachment_id, a.session_id, a.blob_hash, a.original_name,
                        b.size_bytes, b.media_type, a.agent_readable_path, a.state, a.created_at_ms
                 FROM attachments a
                 JOIN attachment_blobs b ON b.blob_hash = a.blob_hash
                 ORDER BY a.created_at_ms, a.attachment_id",
            )
            .map_err(|source| internal_error("attachments could not be queried", source))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .map_err(|source| internal_error("attachments could not be read", source))?;
        rows.map(|row| {
            parse_attachment(
                row.map_err(|source| internal_error("attachment row could not be read", source))?,
            )
        })
        .collect()
    }

    fn attachment_by_blob_hash(
        &self,
        session_id: &SessionId,
        blob_hash: &str,
    ) -> StorageResult<Option<StoredAttachment>> {
        self.query_one_attachment(
            "WHERE a.session_id = ?1 AND a.blob_hash = ?2",
            [session_id.as_str(), blob_hash],
        )
    }

    fn attachment_by_id(
        &self,
        attachment_id: &AttachmentId,
    ) -> StorageResult<Option<StoredAttachment>> {
        self.query_one_attachment(
            "WHERE a.attachment_id = ?1 AND ?2 = ?2",
            [attachment_id.as_str(), attachment_id.as_str()],
        )
    }

    fn query_one_attachment(
        &self,
        predicate: &str,
        values: [&str; 2],
    ) -> StorageResult<Option<StoredAttachment>> {
        let sql = format!(
            "SELECT a.attachment_id, a.session_id, a.blob_hash, a.original_name,
                    b.size_bytes, b.media_type, a.agent_readable_path, a.state, a.created_at_ms
             FROM attachments a
             JOIN attachment_blobs b ON b.blob_hash = a.blob_hash {predicate}"
        );
        self.connection
            .query_row(&sql, values, |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .optional()
            .map_err(|source| internal_error("attachment could not be queried", source))?
            .map(parse_attachment)
            .transpose()
    }

    fn validate_or_repair_attachment(
        &mut self,
        attachment: &StoredAttachment,
    ) -> StorageResult<()> {
        super::filesystem::validate_attachment_component(&attachment.attachment_id)?;
        super::filesystem::validate_session_component(&attachment.session_id)?;
        attachment_io::validate_original_name(&attachment.original_name)?;
        attachment_io::validate_blob_hash(&attachment.blob_hash)?;
        let expected_relative =
            attachment_io::blob_relative_path(&attachment.blob_hash, &attachment.original_name);
        let legacy_relative = attachment_io::legacy_blob_relative_path(&attachment.blob_hash);
        let row_relative = self
            .connection
            .query_row(
                "SELECT relative_path FROM attachment_blobs WHERE blob_hash = ?1",
                [attachment.blob_hash.as_str()],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| {
                internal_error("attachment blob metadata could not be queried", source)
            })?;
        let row_relative = Path::new(&row_relative);
        if row_relative == legacy_relative && legacy_relative != expected_relative {
            self.migrate_legacy_blob(attachment, &legacy_relative, &expected_relative)?;
        } else if row_relative != expected_relative {
            return Err(invalid_data("attachment blob path is invalid"));
        }
        let data_directory = self.blobs_directory.parent().expect("data directory");
        let metadata = fs::symlink_metadata(data_directory.join(expected_relative))
            .map_err(|source| internal_error("attachment blob could not be inspected", source))?;
        if !metadata.file_type().is_file() || metadata.len() != attachment.size_bytes {
            return Err(invalid_data("attachment blob is unavailable"));
        }
        let attachment_directory = self
            .connection
            .query_row(
                "SELECT attachment_directory FROM session_resources WHERE session_id = ?1",
                [attachment.session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| {
                internal_error("attachment directory could not be queried", source)
            })?;
        let expected_view = attachment_io::stable_view_path(
            Path::new(&attachment_directory),
            &attachment.attachment_id,
            &attachment.original_name,
        );
        if Path::new(&attachment.agent_readable_path) != expected_view {
            return Err(invalid_data("attachment readable path is invalid"));
        }
        attachment_io::ensure_stable_view(
            &expected_view,
            &attachment.blob_hash,
            &attachment.original_name,
        )?;
        Ok(())
    }

    fn migrate_legacy_blob(
        &mut self,
        attachment: &StoredAttachment,
        legacy_relative: &Path,
        expected_relative: &Path,
    ) -> StorageResult<()> {
        let data_directory = self.blobs_directory.parent().expect("data directory");
        let legacy_blob = data_directory.join(legacy_relative);
        let expected_blob = data_directory.join(expected_relative);
        let legacy_metadata = fs::symlink_metadata(&legacy_blob);
        let expected_metadata = fs::symlink_metadata(&expected_blob);

        match (legacy_metadata, expected_metadata) {
            (Ok(legacy), Err(source)) if source.kind() == std::io::ErrorKind::NotFound => {
                if !legacy.file_type().is_file() || legacy.len() != attachment.size_bytes {
                    return Err(invalid_data("legacy attachment blob is invalid"));
                }
                fs::rename(&legacy_blob, &expected_blob).map_err(|source| {
                    internal_error("legacy attachment blob could not be migrated", source)
                })?;
                super::sync_directory(
                    expected_blob
                        .parent()
                        .expect("attachment blob has a parent"),
                )?;
            }
            (Err(source), Ok(expected)) if source.kind() == std::io::ErrorKind::NotFound => {
                if !expected.file_type().is_file() || expected.len() != attachment.size_bytes {
                    return Err(invalid_data("migrated attachment blob is invalid"));
                }
            }
            (Ok(_), Ok(_)) => {
                return Err(invalid_data(
                    "legacy and migrated attachment blobs both exist",
                ));
            }
            (Err(source), Err(_)) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(invalid_data("attachment blob is unavailable"));
            }
            (Err(source), _) | (_, Err(source)) => {
                return Err(internal_error(
                    "attachment blob migration state could not be inspected",
                    source,
                ));
            }
        }

        let changed = self
            .connection
            .execute(
                "UPDATE attachment_blobs
                 SET relative_path = ?2
                 WHERE blob_hash = ?1 AND relative_path = ?3",
                params![
                    attachment.blob_hash,
                    path_text(expected_relative)?,
                    path_text(legacy_relative)?,
                ],
            )
            .map_err(|source| {
                database_write_error(
                    "attachment blob path migration could not be committed",
                    source,
                )
            })?;
        if changed != 1 {
            return Err(invalid_data(
                "attachment blob path migration lost authority",
            ));
        }
        Ok(())
    }
}

type AttachmentRow = (
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    String,
    String,
    i64,
);

fn parse_attachment(row: AttachmentRow) -> StorageResult<StoredAttachment> {
    let media_type = row
        .5
        .or_else(|| crate::image::sniff_media_type(Path::new(&row.6)).ok());
    Ok(StoredAttachment {
        attachment_id: AttachmentId::new(row.0).map_err(|source| {
            invalid_data_with_source("stored attachment id is invalid", source)
        })?,
        session_id: SessionId::new(row.1).map_err(|source| {
            invalid_data_with_source("stored attachment session id is invalid", source)
        })?,
        blob_hash: row.2,
        original_name: row.3,
        size_bytes: non_negative_u64(row.4, "stored attachment size is invalid")?,
        media_type,
        agent_readable_path: row.6,
        state: match row.7.as_str() {
            "ready" => StoredAttachmentState::Ready,
            "unavailable" => StoredAttachmentState::Unavailable,
            _ => return Err(invalid_data("stored attachment state is invalid")),
        },
        created_at_ms: row.8,
    })
}

fn path_text(path: &Path) -> StorageResult<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        StoreError::new(
            StoreErrorKind::InvalidInput,
            "attachment path must be UTF-8",
        )
    })
}
