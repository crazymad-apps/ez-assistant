//! 计划后处理与主控回执共用同一 Store worker，文件先落盘，消息提交决定最终归属。

mod files;

#[cfg(test)]
use agent_types::{ConversationMessage, UserMessageOrigin};
use assistant_protocol::{
    ConversationOwner, MessageId, ScheduleDelivery, ScheduleExecutionId, ScheduleId, SessionId,
};
use assistant_runtime::{
    AcceptedInput, ConversationMessageLocationRequest, CrossSessionInputBinding,
    ResolvedScheduleFile, ScheduleMaintenance, ScheduleReportCommit, StoredInputState,
};
use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};

use crate::storage::{
    StorageEngine, StorageResult, conflict, internal_error, invalid_data, invalid_data_with_source,
};

impl StorageEngine {
    pub(in crate::storage) fn next_schedule_maintenance(
        &self,
        after: Option<String>,
    ) -> StorageResult<Option<(String, ScheduleMaintenance)>> {
        let row = self
            .connection
            .query_row(
                "SELECT key, identifier, kind FROM (
                SELECT '0:'||schedule_id AS key, schedule_id AS identifier, 0 AS kind
                FROM schedules WHERE deleting=1
                UNION ALL
                SELECT '1:'||e.execution_id, e.execution_id, 1
                FROM schedule_executions e JOIN schedules s USING(schedule_id)
                WHERE s.deleting=0 AND e.result_finalized=0 AND e.status<>'running'
             ) WHERE key>?1 ORDER BY key LIMIT 1",
                [after.as_deref().unwrap_or("")],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u8>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_error)?;
        row.map(|(key, id, kind)| {
            let work = if kind == 0 {
                ScheduleMaintenance::Deletion(ScheduleId::new(id).map_err(id_error)?)
            } else {
                ScheduleMaintenance::Result(ScheduleExecutionId::new(id).map_err(id_error)?)
            };
            Ok((key, work))
        })
        .transpose()
    }

    pub(in crate::storage) fn mark_schedule_deleting(
        &mut self,
        id: ScheduleId,
    ) -> StorageResult<bool> {
        // 已进入 staged append 的消息由原提交接手。先完成该回执，之后删除标记才与新投递互斥。
        let operations = {
            let mut stmt = self
                .connection
                .prepare(
                    "SELECT b.operation_id FROM body_appends b JOIN schedule_executions e
                   ON b.operation_id='scheduled-report:'||e.execution_id WHERE e.schedule_id=?1",
                )
                .map_err(sql_error)?;
            stmt.query_map([id.as_str()], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?
        };
        for operation in operations {
            self.complete_staged_append(&operation)?;
        }
        let changed = self
            .connection
            .execute(
                "UPDATE schedules SET deleting=1,next_due_at_ms=NULL WHERE schedule_id=?1",
                [id.as_str()],
            )
            .map_err(sql_error)?;
        Ok(changed == 1)
    }

    pub(in crate::storage) fn archive_schedule_result(
        &mut self,
        id: ScheduleExecutionId,
    ) -> StorageResult<()> {
        let Some(exec) = self.get_schedule_execution(id.clone())? else {
            return Ok(());
        };
        let pending: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schedule_executions e JOIN schedules s USING(schedule_id)
                WHERE e.execution_id=?1 AND e.status<>'running' AND e.files_json IS NULL AND s.deleting=0)",
            [id.as_str()], |row| row.get(0),
        ).map_err(sql_error)?;
        if !pending {
            return Ok(());
        }
        let artifacts = exec
            .agent_result
            .as_ref()
            .map_or(&[][..], |result| result.artifacts.as_slice());
        let files = if artifacts.is_empty() {
            Vec::new()
        } else {
            let session = exec
                .agent_session_id
                .ok_or_else(|| invalid_data("scheduled artifact session is missing"))?;
            let environment = self.load_session_environment(&session)?;
            files::archive(
                &self.result_directory(),
                &id,
                &environment,
                artifacts,
                &self.resource_paths,
            )?
        };
        let encoded = serde_json::to_string(&files).map_err(|error| {
            invalid_data_with_source("schedule file manifest could not be encoded", error)
        })?;
        self.connection.execute(
            "UPDATE schedule_executions SET files_json=?2 WHERE execution_id=?1 AND files_json IS NULL",
            params![id.as_str(), encoded],
        ).map_err(sql_error)?;
        Ok(())
    }

    pub(in crate::storage) fn commit_schedule_report(
        &mut self,
        commit: ScheduleReportCommit,
    ) -> StorageResult<Option<AcceptedInput>> {
        let input = *commit.input;
        if !matches!(
            input.cross_session.as_ref().map(|envelope| &envelope.binding),
            Some(CrossSessionInputBinding::ScheduleReport { execution_id }) if execution_id == &commit.execution_id
        ) {
            return Err(invalid_data("schedule report binding is invalid"));
        }
        if let Some((delivery, _)) = self.schedule_delivery(&commit.execution_id)? {
            // 恢复尚未投影的输入。旧通知或已领取的输入不启动新一轮。
            if delivery.session_id == input.session_id
                && delivery.message_id.as_str() == input.message.id.as_str()
                && let Some(stored) = self.load_input_for_id(&input.session_id, &input.input_id)?
                && stored.state == StoredInputState::Queued
            {
                return Ok(Some(AcceptedInput::Control {
                    input: Box::new(stored),
                    run: None,
                    is_duplicate: false,
                }));
            }
            return Ok(None);
        }
        self.ensure_new_input_identity(
            &input.session_id,
            &input.input_id,
            None,
            &input.message.id,
        )?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        // 通用接纳在同一事务中写回执、归档所有权和排队输入。
        crate::storage::input_state::apply_input_acceptance(
            &transaction,
            &input,
            self.allow_legacy_history,
        )?;
        transaction.commit().map_err(sql_error)?;
        let stored = self
            .load_input_for_id(&input.session_id, &input.input_id)?
            .ok_or_else(|| invalid_data("accepted schedule report input is missing"))?;
        Ok(Some(AcceptedInput::Control {
            input: Box::new(stored),
            run: None,
            is_duplicate: false,
        }))
    }

    pub(in crate::storage) fn finish_schedule_result(
        &mut self,
        id: ScheduleExecutionId,
    ) -> StorageResult<()> {
        let owner = self.connection.query_row(
            "SELECT e.session_id, (e.status NOT IN ('outcome_unknown','cleanup_unconfirmed') OR e.cleanup_confirmed=1)
             FROM schedule_executions e JOIN schedules s USING(schedule_id)
             WHERE e.execution_id=?1 AND s.deleting=0 AND e.files_json IS NOT NULL AND e.result_finalized=0
             AND e.status<>'running' 
             AND (e.status='skipped' OR json_extract(e.settings_json,'$.report_to_controller')=0
                  OR EXISTS(SELECT 1 FROM schedule_deliveries d WHERE d.execution_id=e.execution_id))",
            [id.as_str()], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?)),
        ).optional().map_err(sql_error)?;
        let Some((owner, can_release)) = owner else {
            return Ok(());
        };
        if can_release && let Some(owner) = owner {
            self.remove_scheduled_session(SessionId::new(owner).map_err(id_error)?)?;
        }
        self.connection
            .execute(
                "UPDATE schedule_executions SET result_finalized=1 WHERE execution_id=?1",
                [id.as_str()],
            )
            .map_err(sql_error)?;
        Ok(())
    }

    pub(in crate::storage) fn clean_deleting_schedule(
        &mut self,
        id: &ScheduleId,
    ) -> StorageResult<bool> {
        let marked: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schedules WHERE schedule_id=?1 AND deleting=1)",
                [id.as_str()],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !marked {
            return Ok(false);
        }
        let blocked: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM schedule_executions
             WHERE schedule_id=?1 AND status IN ('running','outcome_unknown','cleanup_unconfirmed') AND cleanup_confirmed=0)",
            [id.as_str()], |row| row.get(0),
        ).map_err(sql_error)?;
        if blocked {
            return Ok(false);
        }
        let records = {
            let mut stmt = self
                .connection
                .prepare(
                    "SELECT execution_id,session_id FROM schedule_executions WHERE schedule_id=?1",
                )
                .map_err(sql_error)?;
            stmt.query_map([id.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(sql_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql_error)?
        };
        for (execution, session) in records {
            let execution_id = ScheduleExecutionId::new(execution.clone()).map_err(id_error)?;
            let retained = self.live_delivery_files(&execution_id)?.is_some();
            if let Some(session) = session {
                self.remove_scheduled_session(SessionId::new(session).map_err(id_error)?)?;
            }
            if !retained {
                files::remove_owned_directory(&self.result_directory(), &execution)?;
                files::remove_owned_directory(
                    &self.result_directory(),
                    &format!("staging-{execution}"),
                )?;
            }
        }
        files::remove_owned_directory(&self.data_directory().join("schedules"), id.as_str())?;
        Ok(true)
    }

    pub(in crate::storage) fn resolve_schedule_file(
        &mut self,
        id: ScheduleExecutionId,
        file_id: String,
    ) -> StorageResult<Option<ResolvedScheduleFile>> {
        let archive: Option<String> = self
            .connection
            .query_row(
                "SELECT e.files_json FROM schedule_executions e JOIN schedules s USING(schedule_id)
             WHERE e.execution_id=?1 AND s.deleting=0",
                [id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(sql_error)?
            .flatten();
        let encoded = if let Some(archive) = archive {
            archive
        } else {
            let Some(files) = self.live_delivery_files(&id)? else {
                return Ok(None);
            };
            files
        };
        let manifest = decode_files(&encoded)?;
        let Some(file) = manifest.into_iter().find(|item| item.file_id == file_id) else {
            return Ok(None);
        };
        let path = files::result_path(&self.result_directory(), &id, &file)?;
        Ok(Some(ResolvedScheduleFile { path, file }))
    }

    fn live_delivery_files(&mut self, id: &ScheduleExecutionId) -> StorageResult<Option<String>> {
        let Some((target, files)) = self.schedule_delivery(id)? else {
            return Ok(None);
        };
        let queued: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM inputs WHERE session_id=?1 AND user_message_id=?2
              AND state='queued' AND json_extract(cross_session_json,'$.binding.kind')='schedule_report'
              AND json_extract(cross_session_json,'$.binding.execution_id')=?3)",
            params![target.session_id.as_str(), target.message_id.as_str(), id.as_str()],
            |row| row.get(0),
        ).map_err(sql_error)?;
        if queued {
            return Ok(Some(files));
        }
        let message_id =
            agent_types::MessageId::new(target.message_id.as_str().to_owned()).map_err(id_error)?;
        let location = self.locate_conversation_message(ConversationMessageLocationRequest {
            owner: ConversationOwner::MainSession {
                session_id: target.session_id,
            },
            message_id,
        })?;
        Ok(location
            .and_then(|item| item.display_ordinal)
            .map(|_| files))
    }

    pub(super) fn schedule_delivery(
        &self,
        id: &ScheduleExecutionId,
    ) -> StorageResult<Option<(ScheduleDelivery, String)>> {
        self.connection.query_row(
            "SELECT session_id,message_id,files_json FROM schedule_deliveries WHERE execution_id=?1",
            [id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        ).optional().map_err(sql_error)?.map(|(session, message, files)| {
            Ok((ScheduleDelivery { session_id: SessionId::new(session).map_err(id_error)?,
                message_id: MessageId::new(message).map_err(id_error)? }, files))
        }).transpose()
    }

    fn data_directory(&self) -> &std::path::Path {
        self.blobs_directory
            .parent()
            .expect("blobs are below the data directory")
    }

    fn result_directory(&self) -> std::path::PathBuf {
        self.data_directory().join("scheduled-runs")
    }
}

pub(in crate::storage) fn apply_schedule_report(
    tx: &Transaction<'_>,
    execution_id: &ScheduleExecutionId,
    session: &SessionId,
    message: &MessageId,
) -> StorageResult<()> {
    let changed = tx.execute(
        "INSERT INTO schedule_deliveries(execution_id,session_id,message_id,files_json)
         SELECT e.execution_id,?2,?3,e.files_json FROM schedule_executions e JOIN schedules s USING(schedule_id)
         JOIN sessions target ON target.session_id=?2 AND target.role='controller'
         WHERE e.execution_id=?1 AND s.deleting=0 AND e.status NOT IN ('running','skipped')
           AND e.files_json IS NOT NULL AND json_extract(e.settings_json,'$.report_to_controller')=1",
        params![execution_id.as_str(), session.as_str(), message.as_str()],
    ).map_err(sql_error)?;
    if changed != 1 {
        return Err(conflict("schedule report is not eligible"));
    }
    Ok(())
}

pub(super) fn decode_files(
    encoded: &str,
) -> StorageResult<Vec<assistant_protocol::ScheduleResultFile>> {
    serde_json::from_str(encoded)
        .map_err(|error| invalid_data_with_source("schedule file manifest is invalid", error))
}

fn id_error(
    error: impl std::error::Error + Send + Sync + 'static,
) -> assistant_runtime::StoreError {
    invalid_data_with_source("schedule delivery identity is invalid", error)
}

fn sql_error(error: rusqlite::Error) -> assistant_runtime::StoreError {
    internal_error("schedule delivery could not be stored", error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::append_effect::{AppendPurpose, ConversationStorageTarget};
    use assistant_protocol::{IdempotencyKey, ScheduleExecutionStatus, ScheduleScriptResult};
    use assistant_runtime::{
        ScheduleAdmission, ScheduleAdmitted, ScheduleResult, ScheduleSettlement, ScheduleTrigger,
        StoredSchedule,
    };

    #[test]
    fn report_receipt_recovers_after_file_write_and_survives_schedule_deletion() {
        let home = tempfile::tempdir().unwrap();
        let mut db = StorageEngine::open(home.path()).unwrap();
        let schedule = ScheduleId::new("schedule-fixture").unwrap();
        let id = ScheduleExecutionId::new("execution-fixture").unwrap();
        let settings = serde_json::from_value(serde_json::json!({
            "name":"Fixture","enabled":false,"report_to_controller":true,"rule":{"type":"daily","time":"09:00"},
            "action":{"type":"script","command":"printf fixture"}
        })).unwrap();
        assert!(
            db.insert_schedule(StoredSchedule {
                schedule_id: schedule.clone(),
                deleting: false,
                once_triggered: false,
                settings,
                next_due_at_ms: None,
            })
            .unwrap()
        );
        assert!(matches!(
            db.admit_schedule(ScheduleAdmission {
                schedule_id: schedule.clone(),
                execution_id: id.clone(),
                accepted_at_ms: 2000,
                trigger: ScheduleTrigger::Manual(IdempotencyKey::new("run").unwrap()),
                tracks_script_process: false,
            })
            .unwrap(),
            ScheduleAdmitted::Started(_)
        ));
        db.settle_schedule_execution(ScheduleSettlement {
            execution_id: id.clone(),
            finished_at_ms: 2001,
            status: ScheduleExecutionStatus::Succeeded,
            result: ScheduleResult::Script(ScheduleScriptResult {
                stdout: "fixture".into(),
                ..Default::default()
            }),
        })
        .unwrap();
        db.archive_schedule_result(id.clone()).unwrap();
        let mut controller =
            crate::storage::tests::new_session("s-report-fixture", &db.sessions_directory);
        controller.role = assistant_runtime::SessionRole::Controller;
        let session_id = controller.session_id.clone();
        db.create_session(controller).unwrap();
        let message_id = MessageId::new("report-fixture").unwrap();
        let message = agent_types::UserMessage {
            id: agent_types::MessageId::new(message_id.as_str()).unwrap(),
            origin: UserMessageOrigin::HostNotice,
            transcript_visibility: agent_types::TranscriptVisibility::Visible,
            parts: vec![agent_types::UserPart::Text(agent_types::TextPart {
                id: agent_types::PartId::new("report-part").unwrap(),
                text: "fixture".into(),
            })],
        };
        let operation = format!("scheduled-report:{id}");
        db.stage_target_append(
            operation.clone(),
            ConversationStorageTarget::SessionMetadata {
                session_id: session_id.clone(),
            },
            vec![ConversationMessage::User(message.clone())],
            None,
            2002,
            AppendPurpose::ScheduleReport {
                execution_id: id.clone(),
                message_id: message_id.clone(),
            },
        )
        .unwrap();
        db.write_staged_append(&operation).unwrap();
        assert!(db.schedule_delivery(&id).unwrap().is_none());
        drop(db); // 正文已写入但回执提交前退出，重启要收敛原事务。

        let mut db = StorageEngine::open(home.path()).unwrap();
        db.prepare_session_execution(&session_id).unwrap();
        let target = ScheduleDelivery {
            session_id: session_id.clone(),
            message_id,
        };
        assert_eq!(db.schedule_delivery(&id).unwrap().unwrap().0, target);
        let queued_id = ScheduleExecutionId::new("execution-queued").unwrap();
        db.admit_schedule(ScheduleAdmission {
            schedule_id: schedule.clone(),
            execution_id: queued_id.clone(),
            accepted_at_ms: 2004,
            trigger: ScheduleTrigger::Manual(IdempotencyKey::new("run-next").unwrap()),
            tracks_script_process: false,
        })
        .unwrap();
        db.settle_schedule_execution(ScheduleSettlement {
            execution_id: queued_id.clone(),
            finished_at_ms: 2005,
            status: ScheduleExecutionStatus::Succeeded,
            result: ScheduleResult::Script(ScheduleScriptResult::default()),
        })
        .unwrap();
        db.archive_schedule_result(queued_id.clone()).unwrap();
        let input_id =
            assistant_protocol::InputId::new("scheduled-report-execution-queued").unwrap();
        let mut queued_message = message.clone();
        queued_message.id = agent_types::MessageId::new(input_id.as_str()).unwrap();
        queued_message.origin = UserMessageOrigin::Runtime;
        queued_message
            .parts
            .push(agent_types::UserPart::InternalContext(
                agent_types::InternalContextPart::new(
                    agent_types::PartId::new("queued-source").unwrap(),
                    input_id.as_str(),
                    "schedule_report",
                    "fixture source",
                )
                .unwrap(),
            ));
        let input = assistant_runtime::NewStoredInput {
            input_id: input_id.clone(),
            session_id: session_id.clone(),
            idempotency_key: None,
            agent_variant: assistant_protocol::AgentVariant::Build,
            origin: assistant_runtime::InputOrigin::Runtime,
            goal_binding: None,
            cross_session: Some(assistant_runtime::CrossSessionInputEnvelope {
                binding: CrossSessionInputBinding::ScheduleReport {
                    execution_id: queued_id.clone(),
                },
                reply_route: assistant_runtime::ReplyRoute::SessionDefault,
            }),
            channel_source: None,
            skill_activation: None,
            mcp_selection: None,
            feedback_answer: None,
            agent_shell_target: None,
            message: queued_message,
            new_goal: None,
            resumed_goal: None,
            generated_title: None,
            accepted_at_ms: 2006,
        };
        let queued = db
            .commit_schedule_report(ScheduleReportCommit {
                execution_id: queued_id.clone(),
                input: Box::new(input.clone()),
            })
            .unwrap()
            .unwrap();
        assert_eq!(queued.input_id(), &input_id);
        db.mark_schedule_deleting(schedule.clone()).unwrap();
        drop(db); // 已提交删除标记、尚未清理任何结果。

        let mut db = StorageEngine::open(home.path()).unwrap();
        let (_, next) = db.next_schedule_maintenance(None).unwrap().unwrap();
        assert!(matches!(next, ScheduleMaintenance::Deletion(next) if next == schedule));
        let recovered = db
            .commit_schedule_report(ScheduleReportCommit {
                execution_id: queued_id.clone(),
                input: Box::new(input),
            })
            .unwrap()
            .unwrap();
        assert_eq!(recovered.input_id(), &input_id);
        assert_eq!(db.load_inputs_scoped(Some(&session_id)).unwrap().len(), 1);
        assert!(db.delete_schedule(schedule).unwrap());
        assert!(db.get_schedule_execution(id.clone()).unwrap().is_none());
        assert!(db.live_delivery_files(&queued_id).unwrap().is_some());
        assert_eq!(db.schedule_delivery(&id).unwrap().unwrap().0, target);
        let conversation = db.load_conversation(&session_id).unwrap();
        assert_eq!(conversation.messages, [ConversationMessage::User(message)]);
        assert!(
            conversation
                .project_provider_context()
                .unwrap()
                .messages
                .is_empty()
        );
    }
}
