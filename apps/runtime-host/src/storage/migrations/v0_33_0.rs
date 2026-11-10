//! 组织域由 Host 选择目录，升级后的插件记录保存组织来源。

use super::{Migration, Result, usage_snapshots};
use rusqlite::Connection;

pub(super) fn entry() -> Migration {
    Migration {
        key: "release",
        version: "0.33.0",
        min_compatible_host_version: Some("0.33.0"),
        rebuilds_referenced_tables: false,
        validate_source: usage_snapshots::entry().validate,
        apply: |transaction| {
            transaction.execute_batch("ALTER TABLE plugins ADD COLUMN source_organization_id TEXT
                CHECK(source_organization_id IS NULL OR (source_kind = 'center' AND length(source_organization_id) = 36))")?;
            Ok(())
        },
        validate,
    }
}

fn validate(connection: &Connection) -> Result<()> {
    (usage_snapshots::entry().validate)(connection)?;
    connection.prepare("SELECT source_organization_id FROM plugins LIMIT 0")?;
    Ok(())
}

pub(super) fn model_sources_entry() -> Migration {
    Migration {
        key: "model_selection_sources",
        version: "0.33.0",
        min_compatible_host_version: Some("0.33.0"),
        rebuilds_referenced_tables: false,
        validate_source: validate,
        apply: |transaction| {
            // 旧 managed 只证明来源是中心，无法证明真实服务商。保留引用等待显式重选。
            transaction.execute_batch("
                ALTER TABLE sessions ADD COLUMN model_source TEXT NOT NULL DEFAULT 'local'
                    CHECK(model_source IN ('local', 'organization'));
                ALTER TABLE model_settings ADD COLUMN vision_model_source TEXT NOT NULL DEFAULT 'local'
                    CHECK(vision_model_source IN ('local', 'organization'));
                UPDATE sessions SET model_source = 'organization' WHERE model_provider_instance_id = 'managed';
                UPDATE model_settings SET vision_model_source = 'organization' WHERE vision_provider_instance_id = 'managed';
            ")?;
            Ok(())
        },
        validate: |connection| {
            validate(connection)?;
            connection.prepare("SELECT model_source FROM sessions LIMIT 0")?;
            connection.prepare("SELECT vision_model_source FROM model_settings LIMIT 0")?;
            Ok(())
        },
    }
}

pub(super) fn message_cards_entry() -> Migration {
    Migration {
        key: "message_cards",
        version: "0.33.0",
        min_compatible_host_version: Some("0.33.0"),
        rebuilds_referenced_tables: false,
        validate_source: model_sources_entry().validate,
        apply: |transaction| {
            // Run/child 控制行可释放；卡片只外键绑定稳定 Session，历史身份保留为值。
            transaction.execute_batch("CREATE TABLE message_cards (
                position INTEGER PRIMARY KEY AUTOINCREMENT,
                card_id TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
                child_key TEXT NOT NULL DEFAULT '',
                run_id TEXT,
                anchor_message_id TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                tool_call_id TEXT NOT NULL,
                title TEXT NOT NULL,
                source_json TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                snapshot_json TEXT,
                snapshot_schema TEXT,
                snapshot_revision INTEGER NOT NULL DEFAULT 0 CHECK(snapshot_revision >= 0),
                inherited INTEGER NOT NULL DEFAULT 0 CHECK(inherited IN (0,1)),
                UNIQUE(session_id, child_key, receipt_id, tool_call_id),
                CHECK((snapshot_json IS NULL) = (snapshot_schema IS NULL))
            );
            CREATE INDEX message_cards_owner_position ON message_cards(session_id, child_key, position);
            CREATE INDEX message_cards_anchor ON message_cards(session_id, child_key, anchor_message_id);")?;
            Ok(())
        },
        validate: |connection| {
            (model_sources_entry().validate)(connection)?;
            connection.prepare(
                "SELECT card_id, session_id, child_key, receipt_id, tool_call_id,
                anchor_message_id, source_json, payload_json, snapshot_json, snapshot_schema,
                snapshot_revision, inherited FROM message_cards LIMIT 0",
            )?;
            Ok(())
        },
    }
}
