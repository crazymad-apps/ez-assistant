//! 插件安装事实与操作账本的存储契约；文件发布由 Host 协调。

use assistant_protocol::IdempotencyKey;

use super::{StoreError, StoreErrorKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginSource {
    Upload,
    Center(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPlugin {
    pub name: String,
    pub source: PluginSource,
    pub active_version: Option<String>,
    pub active_sha256: Option<String>,
    pub active_manifest_json: Option<String>,
    pub enabled: bool,
    pub parameters_json: String,
    pub software_paths_json: String,
    pub market_status: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// 设置 CAS 同时核对旧包和旧值；Store 只原子替换设置列。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSettingsChange {
    pub expected: StoredPlugin,
    pub next: StoredPlugin,
}

impl PluginSettingsChange {
    pub fn validate(&self) -> Result<(), StoreError> {
        self.expected.validate()?;
        self.next.validate()?;
        if self.expected.name != self.next.name
            || self.expected.source != self.next.source
            || self.expected.active_sha256 != self.next.active_sha256
            || self.expected.active_version != self.next.active_version
            || self.expected.active_manifest_json != self.next.active_manifest_json
            || self.expected.created_at_ms != self.next.created_at_ms
            || self.next.updated_at_ms < self.expected.updated_at_ms
        {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "plugin package changed",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginOperationKind {
    Install,
    Update,
    Uninstall,
}

impl PluginOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Uninstall => "uninstall",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "install" => Some(Self::Install),
            "update" => Some(Self::Update),
            "uninstall" => Some(Self::Uninstall),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginOperationPhase {
    Accepted,
    Preparing,
    Committing,
    Cleaning,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

impl PluginOperationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Preparing => "preparing",
            Self::Committing => "committing",
            Self::Cleaning => "cleaning",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "preparing" => Some(Self::Preparing),
            "committing" => Some(Self::Committing),
            "cleaning" => Some(Self::Cleaning),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPluginOperation {
    pub key: IdempotencyKey,
    pub kind: PluginOperationKind,
    /// 原始已规范化意图；同 key 必须逐字节一致。
    pub intent_json: String,
    pub phase: PluginOperationPhase,
    pub committed_json: String,
    pub cancel_requested: bool,
    pub error_code: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginRecordChange {
    Keep,
    Upsert(StoredPlugin),
    Remove(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginOperationUpdate {
    pub expected_phase: PluginOperationPhase,
    pub next: StoredPluginOperation,
    pub plugin: PluginRecordChange,
}

impl StoredPluginOperation {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.updated_at_ms < self.created_at_ms
            || !json_object(&self.intent_json)
            || !valid_json(&self.committed_json, true)
            || self
                .error_code
                .as_ref()
                .is_some_and(|value| value.len() > 128)
        {
            return Err(invalid_plugin());
        }
        Ok(())
    }
}

impl PluginOperationUpdate {
    pub fn validate_against(&self, current: &StoredPluginOperation) -> Result<(), StoreError> {
        self.next.validate()?;
        match &self.plugin {
            PluginRecordChange::Keep => {}
            PluginRecordChange::Upsert(plugin) => plugin.validate()?,
            PluginRecordChange::Remove(name) if plugin_name(name) && name.len() <= 64 => {}
            PluginRecordChange::Remove(_) => return Err(invalid_plugin()),
        }
        if current.phase != self.expected_phase
            || current.kind != self.next.kind
            || current.intent_json != self.next.intent_json
            || current.created_at_ms != self.next.created_at_ms
            || self.next.updated_at_ms < current.updated_at_ms
            || matches!(
                current.phase,
                PluginOperationPhase::Succeeded
                    | PluginOperationPhase::Failed
                    | PluginOperationPhase::Cancelled
            )
            || !current.phase.can_advance_to(self.next.phase)
            || (!matches!(self.plugin, PluginRecordChange::Keep)
                && current.phase != PluginOperationPhase::Committing)
        {
            return Err(StoreError::new(
                StoreErrorKind::Conflict,
                "plugin operation state changed",
            ));
        }
        Ok(())
    }
}

impl PluginOperationPhase {
    fn can_advance_to(self, next: Self) -> bool {
        use PluginOperationPhase as Phase;
        match self {
            Phase::Accepted => matches!(
                next,
                Phase::Accepted | Phase::Preparing | Phase::Cancelled | Phase::Failed
            ),
            Phase::Preparing => matches!(
                next,
                Phase::Preparing
                    | Phase::Committing
                    | Phase::Cancelled
                    | Phase::Failed
                    | Phase::Interrupted
            ),
            Phase::Committing => matches!(
                next,
                Phase::Committing | Phase::Cleaning | Phase::Failed | Phase::Interrupted
            ),
            Phase::Cleaning => matches!(
                next,
                Phase::Cleaning
                    | Phase::Preparing
                    | Phase::Succeeded
                    | Phase::Failed
                    | Phase::Interrupted
            ),
            Phase::Interrupted => matches!(
                next,
                Phase::Interrupted
                    | Phase::Preparing
                    | Phase::Committing
                    | Phase::Cleaning
                    | Phase::Cancelled
                    | Phase::Failed
            ),
            Phase::Succeeded | Phase::Failed | Phase::Cancelled => false,
        }
    }
}

impl StoredPlugin {
    pub fn validate(&self) -> Result<(), StoreError> {
        let active = (
            &self.active_version,
            &self.active_sha256,
            &self.active_manifest_json,
        );
        let valid_active = match active {
            (None, None, None) => true,
            (Some(version), Some(sha), Some(manifest)) => {
                !version.is_empty()
                    && sha.len() == 64
                    && sha
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    && json_object(manifest)
            }
            _ => false,
        };
        if !plugin_name(&self.name)
            || self.name.len() > 64
            || self.updated_at_ms < self.created_at_ms
            || !valid_active
            || !json_object(&self.parameters_json)
            || !json_object(&self.software_paths_json)
            || matches!(&self.source, PluginSource::Center(id) if id.is_empty())
        {
            return Err(invalid_plugin());
        }
        Ok(())
    }
}

fn valid_json(value: &str, array: bool) -> bool {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::Array(_)) if array => true,
        Ok(_) if !array => true,
        _ => false,
    }
}

fn json_object(value: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(value),
        Ok(serde_json::Value::Object(_))
    )
}

fn plugin_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn invalid_plugin() -> StoreError {
    StoreError::new(
        StoreErrorKind::InvalidInput,
        "plugin storage value is invalid",
    )
}
