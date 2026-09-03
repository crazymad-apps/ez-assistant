//! `mcp.json` 的解析、规范化、脱敏投影与原子 mutation。

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
    time::Duration,
};

use assistant_protocol::{
    McpConfigurationMutation, McpConfigurationSnapshot, McpDiagnosticCode, McpDiagnosticSnapshot,
    McpImportPreviewEntry, McpServerDraft, McpServerKey, McpServerRuntimeState, McpServerSnapshot,
    McpServerTransportDraft, McpTransportKind, PreviewMcpImportResult,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use url::Url;

use super::{
    McpConfigSource, McpRegistryServerProjection, McpSecret, McpServerConfig,
    McpServerTransportConfig,
};
use crate::{
    ConfigSourceLoad, ConfigSourceReplace, RuntimeError, RuntimeResult, config::McpRuntimeConfig,
};

const ABSENT_REVISION: &str = "absent";
const DEFAULT_DESCRIPTION: &str = "未配置业务范围，请先发现工具";
const MAX_ENABLED_SERVERS: usize = 32;
const MAX_DISPLAY_NAME_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_COMMAND_BYTES: usize = 4 * 1024;
const MAX_ARGUMENTS: usize = 64;
const MAX_SECRET_ENTRIES: usize = 64;
const MAX_SECRET_BYTES: usize = 32 * 1024;
const MAX_STARTUP_TIMEOUT_MS: u64 = 60_000;

pub(crate) struct McpConfigStore {
    source: Arc<dyn McpConfigSource>,
    gate: Mutex<()>,
}

impl McpConfigStore {
    pub(crate) fn new(source: Arc<dyn McpConfigSource>) -> Self {
        Self {
            source,
            gate: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) async fn snapshot(&self) -> RuntimeResult<McpConfigurationSnapshot> {
        self.snapshot_with_runtime(&BTreeMap::new()).await
    }

    pub(crate) async fn snapshot_with_runtime(
        &self,
        runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
    ) -> RuntimeResult<McpConfigurationSnapshot> {
        let _gate = self.gate.lock().await;
        let loaded = self.load().await?;
        Ok(loaded.project(runtime))
    }

    pub(crate) async fn registry_candidate(&self) -> RuntimeResult<McpRegistryCandidate> {
        let _gate = self.gate.lock().await;
        let loaded = self.load().await?;
        Ok(McpRegistryCandidate {
            document_valid: loaded.mutable,
            configured_keys: loaded
                .raw_servers
                .keys()
                .filter_map(|key| McpServerKey::new(key.clone()).ok())
                .collect(),
            servers: loaded
                .servers
                .into_iter()
                .map(|server| (server.server_key.clone(), server))
                .collect(),
            diagnostics: loaded.diagnostics,
        })
    }

    pub(crate) async fn preview_import(
        &self,
        document: &str,
    ) -> RuntimeResult<PreviewMcpImportResult> {
        let _gate = self.gate.lock().await;
        let current = self.load().await?;
        let imported = parse_import(document);
        let mut entries = Vec::new();
        let mut diagnostics = imported.diagnostics;
        for (key, entry) in imported.servers {
            match entry {
                Ok(mut server) => {
                    if !imported.top_level.is_empty() {
                        server.warnings.push(
                            "Unknown top-level fields are preserved but do not affect MCP behavior"
                                .to_owned(),
                        );
                    }
                    entries.push(McpImportPreviewEntry {
                        server_key: key.clone(),
                        display_name: server.config.display_name.clone(),
                        transport: server.config.transport_kind(),
                        conflicts_with_existing: current.raw_servers.contains_key(key.as_str()),
                        warnings: server.warnings,
                    });
                }
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }
        Ok(PreviewMcpImportResult {
            entries,
            diagnostics,
        })
    }

    #[cfg(test)]
    pub(crate) async fn mutate(
        &self,
        expected_revision: &str,
        mutation: McpConfigurationMutation,
    ) -> RuntimeResult<McpConfigurationSnapshot> {
        self.mutate_with_runtime(expected_revision, mutation, &BTreeMap::new())
            .await
    }

    pub(crate) async fn mutate_with_runtime(
        &self,
        expected_revision: &str,
        mutation: McpConfigurationMutation,
        runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
    ) -> RuntimeResult<McpConfigurationSnapshot> {
        let _gate = self.gate.lock().await;
        let current = self.load().await?;
        if current.public_revision() != expected_revision {
            return Err(RuntimeError::McpConfigConflict);
        }
        if !current.mutable {
            return Err(RuntimeError::McpConfigInvalid);
        }

        let mut root = current.root;
        let mut servers = current.raw_servers;
        match mutation {
            McpConfigurationMutation::Upsert { server } => {
                let existing = servers
                    .get(server.server_key.as_str())
                    .and_then(Value::as_object);
                let raw = draft_to_raw(&server, existing)?;
                let normalized = normalize_server(&server.server_key, raw.clone())
                    .map_err(|_| RuntimeError::McpConfigInvalid)?;
                servers.insert(server.server_key.to_string(), Value::Object(normalized.raw));
            }
            McpConfigurationMutation::SetEnabled {
                server_key,
                enabled,
            } => {
                let Some(raw) = servers
                    .get_mut(server_key.as_str())
                    .and_then(Value::as_object_mut)
                else {
                    return Err(RuntimeError::McpConfigInvalid);
                };
                raw.insert("enabled".to_owned(), Value::Bool(enabled));
            }
            McpConfigurationMutation::Remove { server_key } => {
                if servers.remove(server_key.as_str()).is_none() {
                    return Err(RuntimeError::McpConfigInvalid);
                }
            }
            McpConfigurationMutation::Import {
                document,
                replace_server_keys,
            } => {
                let imported = parse_import(&document);
                if !imported.diagnostics.is_empty() {
                    return Err(RuntimeError::McpConfigInvalid);
                }
                for (name, value) in imported.top_level {
                    root.insert(name, value);
                }
                let replacements = replace_server_keys.into_iter().collect::<BTreeSet<_>>();
                for replacement in &replacements {
                    if !servers.contains_key(replacement.as_str())
                        || !imported.servers.contains_key(replacement)
                    {
                        return Err(RuntimeError::McpImportConflict);
                    }
                }
                for (key, entry) in imported.servers {
                    let server = entry.map_err(|_| RuntimeError::McpConfigInvalid)?;
                    if servers.contains_key(key.as_str()) && !replacements.contains(&key) {
                        continue;
                    }
                    servers.insert(key.to_string(), Value::Object(server.raw));
                }
            }
        }
        validate_all_servers(&servers)?;
        root.insert(
            "mcpServers".to_owned(),
            Value::Object(servers.clone().into_iter().collect()),
        );
        let candidate = serde_json::to_string_pretty(&Value::Object(root)).map_err(|_| {
            RuntimeError::InternalStateUnavailable {
                component: "MCP configuration serialization",
            }
        })? + "\n";
        let expected = current.revision;
        match self.source.replace(expected, candidate).await {
            ConfigSourceReplace::Applied(document) => Ok(parse_document(
                document.contents(),
                Some(document.revision().to_owned()),
            )
            .project(runtime)),
            ConfigSourceReplace::Conflict(_) => Err(RuntimeError::McpConfigConflict),
            ConfigSourceReplace::Unavailable(_) => {
                Err(RuntimeError::ConfigurationPersistenceFailed)
            }
        }
    }

    pub(crate) async fn resolve_draft(
        &self,
        draft: &McpServerDraft,
    ) -> RuntimeResult<McpServerConfig> {
        let _gate = self.gate.lock().await;
        let current = self.load().await?;
        let existing = current
            .raw_servers
            .get(draft.server_key.as_str())
            .and_then(Value::as_object);
        let raw = draft_to_raw(draft, existing)?;
        Ok(normalize_server(&draft.server_key, raw)
            .map_err(|_| RuntimeError::McpConfigInvalid)?
            .config)
    }

    async fn load(&self) -> RuntimeResult<ParsedDocument> {
        match self.source.load().await {
            ConfigSourceLoad::Missing => Ok(ParsedDocument::empty()),
            ConfigSourceLoad::Document(document) => Ok(parse_document(
                document.contents(),
                Some(document.revision().to_owned()),
            )),
            ConfigSourceLoad::Unavailable(_) => Err(RuntimeError::ConfigurationUnavailable),
        }
    }
}

pub(crate) struct McpRegistryCandidate {
    pub(crate) document_valid: bool,
    pub(crate) configured_keys: BTreeSet<McpServerKey>,
    pub(crate) servers: BTreeMap<McpServerKey, McpServerConfig>,
    pub(crate) diagnostics: Vec<McpDiagnosticSnapshot>,
}

struct ParsedDocument {
    revision: Option<String>,
    root: Map<String, Value>,
    raw_servers: BTreeMap<String, Value>,
    servers: Vec<McpServerConfig>,
    diagnostics: Vec<McpDiagnosticSnapshot>,
    mutable: bool,
}

impl ParsedDocument {
    fn empty() -> Self {
        let mut root = Map::new();
        root.insert("mcpServers".to_owned(), Value::Object(Map::new()));
        Self {
            revision: None,
            root,
            raw_servers: BTreeMap::new(),
            servers: Vec::new(),
            diagnostics: Vec::new(),
            mutable: true,
        }
    }

    fn public_revision(&self) -> &str {
        self.revision.as_deref().unwrap_or(ABSENT_REVISION)
    }

    fn project(
        self,
        runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
    ) -> McpConfigurationSnapshot {
        let removed = runtime
            .keys()
            .any(|key| !self.raw_servers.contains_key(key.as_str()));
        let revision = self.public_revision().to_owned();
        let servers: Vec<_> = self
            .servers
            .into_iter()
            .map(|server| project_server(server, runtime))
            .collect();
        McpConfigurationSnapshot {
            revision,
            needs_refresh: removed || servers.iter().any(|server| server.needs_refresh),
            servers,
            diagnostics: self.diagnostics,
        }
    }
}

struct ImportedDocument {
    servers: BTreeMap<McpServerKey, Result<NormalizedServer, McpDiagnosticSnapshot>>,
    diagnostics: Vec<McpDiagnosticSnapshot>,
    top_level: Map<String, Value>,
}

struct NormalizedServer {
    raw: Map<String, Value>,
    config: McpServerConfig,
    warnings: Vec<String>,
}

fn parse_document(contents: &str, revision: Option<String>) -> ParsedDocument {
    let value = match serde_json::from_str::<Value>(contents) {
        Ok(value) => value,
        Err(_) => {
            let mut document = ParsedDocument::empty();
            document.revision = revision;
            document.diagnostics.push(diagnostic(
                None,
                None,
                "MCP configuration is not valid JSON",
            ));
            document.mutable = false;
            return document;
        }
    };
    let Value::Object(mut root) = value else {
        let mut document = ParsedDocument::empty();
        document.revision = revision;
        document.diagnostics.push(diagnostic(
            None,
            None,
            "MCP configuration root must be an object",
        ));
        document.mutable = false;
        return document;
    };
    let Some(Value::Object(server_values)) = root.get("mcpServers") else {
        let mut document = ParsedDocument::empty();
        document.revision = revision;
        document.root = root;
        document.diagnostics.push(diagnostic(
            None,
            Some("mcpServers"),
            "MCP configuration must contain an mcpServers object",
        ));
        document.mutable = false;
        return document;
    };
    let server_values = server_values.clone();
    let mut raw_servers = BTreeMap::new();
    let mut servers = Vec::new();
    let mut diagnostics = root
        .keys()
        .filter(|name| name.as_str() != "mcpServers")
        .map(|name| unknown_field_diagnostic(None, name))
        .collect::<Vec<_>>();
    for (raw_key, value) in &server_values {
        raw_servers.insert(raw_key.clone(), value.clone());
        let key = match McpServerKey::new(raw_key.clone()) {
            Ok(key) => key,
            Err(_) => {
                diagnostics.push(diagnostic(
                    None,
                    Some("mcpServers"),
                    "MCP server key is invalid",
                ));
                continue;
            }
        };
        let Value::Object(raw) = value else {
            diagnostics.push(diagnostic(
                Some(key),
                None,
                "MCP server configuration must be an object",
            ));
            continue;
        };
        match normalize_server(&key, raw.clone()) {
            Ok(server) => {
                diagnostics.extend(server.warnings.iter().map(|warning| McpDiagnosticSnapshot {
                    server_key: Some(key.clone()),
                    code: McpDiagnosticCode::UnknownField,
                    field_path: None,
                    message: warning.clone(),
                }));
                servers.push(server.config);
            }
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }
    if servers.iter().filter(|server| server.enabled()).count() > MAX_ENABLED_SERVERS {
        diagnostics.push(diagnostic(
            None,
            Some("mcpServers"),
            "MCP configuration exceeds the server limit",
        ));
    }
    root.insert(
        "mcpServers".to_owned(),
        Value::Object(server_values.clone()),
    );
    ParsedDocument {
        revision,
        root,
        raw_servers,
        servers,
        diagnostics,
        mutable: true,
    }
}

fn parse_import(contents: &str) -> ImportedDocument {
    let value = match serde_json::from_str::<Value>(contents) {
        Ok(Value::Object(value)) => value,
        _ => {
            return ImportedDocument {
                servers: BTreeMap::new(),
                diagnostics: vec![diagnostic(None, None, "MCP import must be a JSON object")],
                top_level: Map::new(),
            };
        }
    };
    let (server_values, top_level) = match value.get("mcpServers") {
        Some(Value::Object(servers)) => (
            servers.clone(),
            value
                .into_iter()
                .filter(|(key, _)| key != "mcpServers")
                .collect(),
        ),
        Some(_) => {
            return ImportedDocument {
                servers: BTreeMap::new(),
                diagnostics: vec![diagnostic(
                    None,
                    Some("mcpServers"),
                    "mcpServers must be an object",
                )],
                top_level: Map::new(),
            };
        }
        None => (value, Map::new()),
    };
    let mut diagnostics = Vec::new();
    let mut servers = BTreeMap::new();
    for (raw_key, value) in server_values {
        let Ok(key) = McpServerKey::new(raw_key) else {
            diagnostics.push(diagnostic(
                None,
                Some("mcpServers"),
                "MCP server key is invalid",
            ));
            continue;
        };
        let result = match value {
            Value::Object(mut raw) => {
                let hint = remove_transport_hint(&mut raw);
                normalize_server(&key, raw).and_then(|mut server| {
                    validate_transport_hint(&key, hint.as_deref(), server.config.transport_kind())?;
                    server.raw.remove("type");
                    server.raw.remove("transport");
                    Ok(server)
                })
            }
            _ => Err(diagnostic(
                Some(key.clone()),
                None,
                "MCP server configuration must be an object",
            )),
        };
        servers.insert(key, result);
    }
    if servers
        .values()
        .filter(|server| server.as_ref().is_ok_and(|server| server.config.enabled()))
        .count()
        > MAX_ENABLED_SERVERS
    {
        diagnostics.push(diagnostic(
            None,
            Some("mcpServers"),
            "MCP import exceeds the enabled server limit",
        ));
    }
    ImportedDocument {
        servers,
        diagnostics,
        top_level,
    }
}

fn normalize_server(
    key: &McpServerKey,
    raw: Map<String, Value>,
) -> Result<NormalizedServer, McpDiagnosticSnapshot> {
    let fingerprint = Sha256::digest(serde_json::to_vec(&raw).map_err(|_| {
        diagnostic(
            Some(key.clone()),
            None,
            "MCP server configuration cannot be normalized",
        )
    })?)
    .into();
    let enabled = optional_bool(&raw, "enabled", true, key)?;
    let display_name = optional_string(&raw, "displayName", key.as_str(), key)?;
    validate_nonempty_bounded(&display_name, MAX_DISPLAY_NAME_BYTES, key, "displayName")?;
    let description = optional_string(&raw, "description", DEFAULT_DESCRIPTION, key)?;
    if description.len() > MAX_DESCRIPTION_BYTES {
        return Err(field_diagnostic(
            key,
            "description",
            "MCP description is too large",
        ));
    }
    let startup_timeout = optional_timeout(&raw, "startupTimeoutMs", MAX_STARTUP_TIMEOUT_MS, key)?;
    let tool_timeout = optional_timeout(
        &raw,
        "toolTimeoutMs",
        McpRuntimeConfig::MAX_TOOL_TIMEOUT_INTEGER_MS,
        key,
    )?;
    let command = raw.get("command");
    let url = raw.get("url");
    let transport = match (command, url) {
        (Some(Value::String(command)), None) => {
            validate_nonempty_bounded(command, MAX_COMMAND_BYTES, key, "command")?;
            if command.contains('\0') {
                return Err(field_diagnostic(key, "command", "MCP command contains NUL"));
            }
            let args = string_array(&raw, "args", key)?;
            if args.len() > MAX_ARGUMENTS || args.iter().any(|value| value.contains('\0')) {
                return Err(field_diagnostic(
                    key,
                    "args",
                    "MCP arguments are invalid or too many",
                ));
            }
            let cwd = optional_nullable_string(&raw, "cwd", key)?;
            if cwd
                .as_deref()
                .is_some_and(|value| !Path::new(value).is_absolute())
            {
                return Err(field_diagnostic(key, "cwd", "MCP cwd must be absolute"));
            }
            let environment = secret_map(&raw, "env", true, key)?;
            McpServerTransportConfig::Stdio {
                command: command.clone(),
                args,
                cwd,
                environment,
            }
        }
        (None, Some(Value::String(url))) => {
            validate_http_url(url, key)?;
            let headers = secret_map(&raw, "headers", false, key)?;
            McpServerTransportConfig::StreamableHttp {
                url: url.clone(),
                headers,
            }
        }
        (Some(_), Some(_)) => {
            return Err(diagnostic(
                Some(key.clone()),
                None,
                "MCP server cannot contain both command and url",
            ));
        }
        (None, None) => {
            return Err(diagnostic(
                Some(key.clone()),
                None,
                "MCP server must contain command or url",
            ));
        }
        _ => {
            return Err(diagnostic(
                Some(key.clone()),
                None,
                "MCP command or url must be a string",
            ));
        }
    };
    let known = [
        "enabled",
        "displayName",
        "description",
        "command",
        "args",
        "cwd",
        "env",
        "url",
        "headers",
        "startupTimeoutMs",
        "toolTimeoutMs",
        "type",
        "transport",
    ];
    let warnings = raw
        .keys()
        .filter(|name| !known.contains(&name.as_str()))
        .map(|name| format!("Unknown field `{name}` is preserved but does not affect MCP behavior"))
        .collect();
    Ok(NormalizedServer {
        raw,
        config: McpServerConfig {
            server_key: key.clone(),
            display_name,
            description,
            enabled,
            transport,
            startup_timeout,
            tool_timeout,
            fingerprint,
        },
        warnings,
    })
}

fn draft_to_raw(
    draft: &McpServerDraft,
    existing: Option<&Map<String, Value>>,
) -> RuntimeResult<Map<String, Value>> {
    let mut raw = existing.cloned().unwrap_or_default();
    for name in ["type", "transport"] {
        raw.remove(name);
    }
    raw.insert("enabled".to_owned(), Value::Bool(draft.enabled));
    raw.insert(
        "displayName".to_owned(),
        Value::String(draft.display_name.clone()),
    );
    raw.insert(
        "description".to_owned(),
        Value::String(draft.description.clone()),
    );
    set_optional_u64(&mut raw, "startupTimeoutMs", draft.startup_timeout_ms);
    set_optional_u64(&mut raw, "toolTimeoutMs", draft.tool_timeout_ms);
    match &draft.transport {
        McpServerTransportDraft::Stdio {
            command,
            args,
            cwd,
            environment,
        } => {
            raw.remove("url");
            raw.remove("headers");
            apply_field_change(&mut raw, "command", command)?;
            apply_field_change(&mut raw, "args", args)?;
            apply_field_change(&mut raw, "cwd", cwd)?;
            let existing_values = existing
                .and_then(|value| value.get("env"))
                .and_then(Value::as_object);
            raw.insert(
                "env".to_owned(),
                Value::Object(apply_secret_changes(existing_values, environment)?),
            );
        }
        McpServerTransportDraft::StreamableHttp { url, headers } => {
            for name in ["command", "args", "cwd", "env"] {
                raw.remove(name);
            }
            apply_field_change(&mut raw, "url", url)?;
            let existing_values = existing
                .and_then(|value| value.get("headers"))
                .and_then(Value::as_object);
            raw.insert(
                "headers".to_owned(),
                Value::Object(apply_secret_changes(existing_values, headers)?),
            );
        }
    }
    Ok(raw)
}

fn apply_secret_changes(
    existing: Option<&Map<String, Value>>,
    changes: &BTreeMap<String, assistant_protocol::McpSecretChange>,
) -> RuntimeResult<Map<String, Value>> {
    let mut values = existing.cloned().unwrap_or_default();
    for (name, change) in changes {
        match change {
            assistant_protocol::McpSecretChange::Keep => {
                if !values.get(name).is_some_and(Value::is_string) {
                    return Err(RuntimeError::InvalidRequest {
                        reason: "MCP secret cannot be kept because no existing value is available",
                    });
                }
            }
            assistant_protocol::McpSecretChange::Replace(secret) => {
                values.insert(name.clone(), Value::String(secret.expose().to_owned()));
            }
            assistant_protocol::McpSecretChange::Remove => {
                values.remove(name);
            }
        }
    }
    Ok(values)
}

fn apply_field_change<T: serde::Serialize>(
    raw: &mut Map<String, Value>,
    name: &str,
    change: &assistant_protocol::McpFieldChange<T>,
) -> RuntimeResult<()> {
    match change {
        assistant_protocol::McpFieldChange::Keep => {}
        assistant_protocol::McpFieldChange::Remove => {
            raw.remove(name);
        }
        assistant_protocol::McpFieldChange::Replace(value) => {
            raw.insert(
                name.to_owned(),
                serde_json::to_value(value).map_err(|_| RuntimeError::McpConfigInvalid)?,
            );
        }
    }
    Ok(())
}

fn validate_all_servers(servers: &BTreeMap<String, Value>) -> RuntimeResult<()> {
    let mut enabled = 0usize;
    for (raw_key, value) in servers {
        let key = McpServerKey::new(raw_key.clone()).map_err(|_| RuntimeError::McpConfigInvalid)?;
        let raw = value.as_object().ok_or(RuntimeError::McpConfigInvalid)?;
        let server =
            normalize_server(&key, raw.clone()).map_err(|_| RuntimeError::McpConfigInvalid)?;
        enabled = enabled.saturating_add(usize::from(server.config.enabled()));
        if enabled > MAX_ENABLED_SERVERS {
            return Err(RuntimeError::McpConfigInvalid);
        }
    }
    Ok(())
}

fn project_server(
    server: McpServerConfig,
    runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
) -> McpServerSnapshot {
    let transport_kind = server.transport_kind();
    let applied = runtime.get(&server.server_key);
    let (target_summary, environment_keys, header_keys) = match &server.transport {
        McpServerTransportConfig::Stdio {
            command,
            environment,
            ..
        } => (
            command.clone(),
            environment.keys().cloned().collect(),
            Vec::new(),
        ),
        McpServerTransportConfig::StreamableHttp { url, headers } => (
            safe_url_summary(url),
            Vec::new(),
            headers.keys().cloned().collect(),
        ),
    };
    McpServerSnapshot {
        server_key: server.server_key,
        display_name: server.display_name,
        description: server.description,
        transport: transport_kind,
        enabled: server.enabled,
        runtime_state: if !server.enabled {
            McpServerRuntimeState::Disabled
        } else {
            applied.map_or(McpServerRuntimeState::Unavailable, |state| state.state)
        },
        tool_count: applied.map_or(0, |state| state.tool_count),
        needs_refresh: if !server.enabled {
            applied.is_some()
        } else {
            applied.is_none_or(|state| state.fingerprint != server.fingerprint)
        },
        target_summary,
        startup_timeout_ms: server
            .startup_timeout
            .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX)),
        tool_timeout_ms: server
            .tool_timeout
            .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX)),
        environment_keys,
        header_keys,
    }
}

fn optional_bool(
    raw: &Map<String, Value>,
    name: &'static str,
    default: bool,
    key: &McpServerKey,
) -> Result<bool, McpDiagnosticSnapshot> {
    match raw.get(name) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(field_diagnostic(key, name, "MCP field must be a boolean")),
    }
}

fn optional_string(
    raw: &Map<String, Value>,
    name: &'static str,
    default: &str,
    key: &McpServerKey,
) -> Result<String, McpDiagnosticSnapshot> {
    match raw.get(name) {
        None => Ok(default.to_owned()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(field_diagnostic(key, name, "MCP field must be a string")),
    }
}

fn optional_nullable_string(
    raw: &Map<String, Value>,
    name: &'static str,
    key: &McpServerKey,
) -> Result<Option<String>, McpDiagnosticSnapshot> {
    match raw.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && !value.contains('\0') => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(field_diagnostic(
            key,
            name,
            "MCP field must be a non-empty string",
        )),
    }
}

fn optional_timeout(
    raw: &Map<String, Value>,
    name: &'static str,
    maximum: u64,
    key: &McpServerKey,
) -> Result<Option<Duration>, McpDiagnosticSnapshot> {
    match raw.get(name) {
        None => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .filter(|value| (1..=maximum).contains(value))
            .map(Duration::from_millis)
            .map(Some)
            .ok_or_else(|| {
                field_diagnostic(
                    key,
                    name,
                    "MCP timeout is outside its supported integer range",
                )
            }),
        Some(_) => Err(field_diagnostic(
            key,
            name,
            "MCP timeout must be an integer",
        )),
    }
}

fn string_array(
    raw: &Map<String, Value>,
    name: &'static str,
    key: &McpServerKey,
) -> Result<Vec<String>, McpDiagnosticSnapshot> {
    match raw.get(name) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(value) => Ok(value.clone()),
                _ => Err(field_diagnostic(key, name, "MCP arguments must be strings")),
            })
            .collect(),
        Some(_) => Err(field_diagnostic(
            key,
            name,
            "MCP arguments must be an array",
        )),
    }
}

fn secret_map(
    raw: &Map<String, Value>,
    name: &'static str,
    environment: bool,
    key: &McpServerKey,
) -> Result<BTreeMap<String, McpSecret>, McpDiagnosticSnapshot> {
    let Some(value) = raw.get(name) else {
        return Ok(BTreeMap::new());
    };
    let Value::Object(values) = value else {
        return Err(field_diagnostic(
            key,
            name,
            "MCP secret map must be an object",
        ));
    };
    if values.len() > MAX_SECRET_ENTRIES {
        return Err(field_diagnostic(
            key,
            name,
            "MCP secret map has too many entries",
        ));
    }
    let mut total = 0usize;
    let mut result = BTreeMap::new();
    for (entry_name, value) in values {
        let Value::String(value) = value else {
            return Err(field_diagnostic(
                key,
                name,
                "MCP secret values must be strings",
            ));
        };
        if value.contains(['\r', '\n', '\0']) {
            return Err(field_diagnostic(
                key,
                name,
                "MCP secret value contains control bytes",
            ));
        }
        if environment {
            if !valid_environment_name(entry_name) {
                return Err(field_diagnostic(
                    key,
                    name,
                    "MCP environment name is invalid",
                ));
            }
        } else {
            validate_header_name(entry_name)
                .map_err(|message| field_diagnostic(key, name, message))?;
        }
        total = total
            .saturating_add(entry_name.len())
            .saturating_add(value.len());
        result.insert(entry_name.clone(), McpSecret::new(value.clone()));
    }
    if total > MAX_SECRET_BYTES {
        return Err(field_diagnostic(key, name, "MCP secret map is too large"));
    }
    Ok(result)
}

fn validate_http_url(value: &str, key: &McpServerKey) -> Result<(), McpDiagnosticSnapshot> {
    let url = Url::parse(value).map_err(|_| field_diagnostic(key, "url", "MCP URL is invalid"))?;
    if url.username() != "" || url.password().is_some() || url.fragment().is_some() {
        return Err(field_diagnostic(
            key,
            "url",
            "MCP URL cannot contain userinfo or fragment",
        ));
    }
    let host = url.host_str().unwrap_or_default();
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(field_diagnostic(
            key,
            "url",
            "MCP URL must use HTTPS unless it targets loopback",
        ));
    }
    Ok(())
}

fn safe_url_summary(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "invalid URL".to_owned();
    };
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn validate_header_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err("MCP HTTP header name is invalid");
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "host" | "content-length" | "accept" | "content-type"
    ) || lower.starts_with("mcp-")
    {
        return Err("MCP HTTP protocol header cannot be overridden");
    }
    Ok(())
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_nonempty_bounded(
    value: &str,
    maximum: usize,
    key: &McpServerKey,
    field: &'static str,
) -> Result<(), McpDiagnosticSnapshot> {
    if value.is_empty() || value.len() > maximum {
        Err(field_diagnostic(
            key,
            field,
            "MCP string is empty or exceeds its size limit",
        ))
    } else {
        Ok(())
    }
}

fn remove_transport_hint(raw: &mut Map<String, Value>) -> Option<String> {
    raw.get("transport")
        .or_else(|| raw.get("type"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn validate_transport_hint(
    key: &McpServerKey,
    hint: Option<&str>,
    actual: McpTransportKind,
) -> Result<(), McpDiagnosticSnapshot> {
    let Some(hint) = hint else {
        return Ok(());
    };
    let hinted = match hint {
        "local" | "stdio" => McpTransportKind::Stdio,
        "remote" | "http" | "streamable-http" | "streamable_http" => {
            McpTransportKind::StreamableHttp
        }
        "sse" | "http+sse" => {
            return Err(field_diagnostic(
                key,
                "transport",
                "Legacy HTTP+SSE transport is not supported",
            ));
        }
        _ => {
            return Err(field_diagnostic(
                key,
                "transport",
                "MCP transport hint is not supported",
            ));
        }
    };
    if hinted != actual {
        return Err(field_diagnostic(
            key,
            "transport",
            "MCP transport hint conflicts with command or url",
        ));
    }
    Ok(())
}

fn set_optional_u64(raw: &mut Map<String, Value>, name: &str, value: Option<u64>) {
    match value {
        Some(value) => {
            raw.insert(name.to_owned(), Value::Number(value.into()));
        }
        None => {
            raw.remove(name);
        }
    }
}

fn field_diagnostic(
    key: &McpServerKey,
    field: &'static str,
    message: &'static str,
) -> McpDiagnosticSnapshot {
    diagnostic(Some(key.clone()), Some(field), message)
}

fn diagnostic(
    server_key: Option<McpServerKey>,
    field_path: Option<&str>,
    message: &'static str,
) -> McpDiagnosticSnapshot {
    McpDiagnosticSnapshot {
        server_key,
        code: McpDiagnosticCode::InvalidConfig,
        field_path: field_path.map(str::to_owned),
        message: message.to_owned(),
    }
}

fn unknown_field_diagnostic(
    server_key: Option<McpServerKey>,
    field: &str,
) -> McpDiagnosticSnapshot {
    McpDiagnosticSnapshot {
        server_key,
        code: McpDiagnosticCode::UnknownField,
        field_path: Some(field.to_owned()),
        message: "Unknown field is preserved but does not affect MCP behavior".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfigDocument, ConfigSourceFuture, ConfigSourceReplaceFuture, McpConfigSource};
    use std::sync::RwLock;

    struct MemorySource {
        document: RwLock<Option<(String, String)>>,
    }

    impl MemorySource {
        fn new(document: Option<&str>) -> Self {
            Self {
                document: RwLock::new(
                    document.map(|document| (document.to_owned(), "revision-1".to_owned())),
                ),
            }
        }
    }

    impl McpConfigSource for MemorySource {
        fn load(&self) -> ConfigSourceFuture<'_> {
            let value = self.document.read().expect("source");
            Box::pin(std::future::ready(match value.as_ref() {
                Some((contents, revision)) => ConfigSourceLoad::Document(ConfigDocument::new(
                    contents.clone(),
                    revision.clone(),
                )),
                None => ConfigSourceLoad::Missing,
            }))
        }

        fn replace(
            &self,
            expected_revision: Option<String>,
            document: String,
        ) -> ConfigSourceReplaceFuture<'_> {
            let mut value = self.document.write().expect("source");
            let current = value.as_ref().map(|(_, revision)| revision.clone());
            let result = if current != expected_revision {
                ConfigSourceReplace::Conflict(match value.as_ref() {
                    Some((contents, revision)) => ConfigSourceLoad::Document(ConfigDocument::new(
                        contents.clone(),
                        revision.clone(),
                    )),
                    None => ConfigSourceLoad::Missing,
                })
            } else {
                let revision = "revision-2".to_owned();
                *value = Some((document.clone(), revision.clone()));
                ConfigSourceReplace::Applied(ConfigDocument::new(document, revision))
            };
            Box::pin(std::future::ready(result))
        }
    }

    #[tokio::test]
    async fn missing_document_starts_empty_and_first_write_uses_absent_revision() {
        let store = McpConfigStore::new(Arc::new(MemorySource::new(None)));
        let snapshot = store.snapshot().await.expect("snapshot");
        assert_eq!(snapshot.revision, ABSENT_REVISION);
        assert!(snapshot.servers.is_empty());

        let result = store
            .mutate(
                ABSENT_REVISION,
                McpConfigurationMutation::Upsert {
                    server: McpServerDraft {
                        server_key: McpServerKey::new("local").expect("key"),
                        display_name: "Local".to_owned(),
                        description: "Tools".to_owned(),
                        enabled: true,
                        transport: McpServerTransportDraft::Stdio {
                            command: assistant_protocol::McpFieldChange::Replace(
                                "server".to_owned(),
                            ),
                            args: assistant_protocol::McpFieldChange::Replace(Vec::new()),
                            cwd: assistant_protocol::McpFieldChange::Replace("/tmp".to_owned()),
                            environment: BTreeMap::new(),
                        },
                        startup_timeout_ms: None,
                        tool_timeout_ms: None,
                    },
                },
            )
            .await
            .expect("create");
        assert_eq!(result.revision, "revision-2");
        assert_eq!(result.servers.len(), 1);
    }

    #[tokio::test]
    async fn import_preserves_unknown_top_level_fields_and_requires_conflict_confirmation() {
        let source = Arc::new(MemorySource::new(Some(
            r#"{"mcpServers":{"existing":{"command":"server"}}}"#,
        )));
        let store = McpConfigStore::new(source.clone());
        let document = r#"{"editorExtension":{"version":1},"mcpServers":{"existing":{"command":"replacement"},"new":{"command":"new-server"}}}"#;
        let preview = store.preview_import(document).await.expect("preview");
        assert!(preview.diagnostics.is_empty());
        assert!(
            preview
                .entries
                .iter()
                .all(|entry| !entry.warnings.is_empty())
        );

        let skipped = store
            .mutate(
                "revision-1",
                McpConfigurationMutation::Import {
                    document: document.to_owned(),
                    replace_server_keys: Vec::new(),
                },
            )
            .await
            .expect("unconfirmed conflicts are skipped");
        assert_eq!(skipped.servers.len(), 2);
        assert_eq!(
            skipped
                .servers
                .iter()
                .find(|server| server.server_key.as_str() == "existing")
                .expect("existing")
                .target_summary,
            "server"
        );

        store
            .mutate(
                &skipped.revision,
                McpConfigurationMutation::Import {
                    document: document.to_owned(),
                    replace_server_keys: vec![McpServerKey::new("existing").expect("key")],
                },
            )
            .await
            .expect("confirmed import");
        let guard = source.document.read().expect("source");
        let value: Value =
            serde_json::from_str(&guard.as_ref().expect("document").0).expect("stored JSON");
        assert_eq!(value["editorExtension"]["version"], 1);
        assert_eq!(value["mcpServers"]["existing"]["command"], "replacement");
        assert_eq!(value["mcpServers"]["new"]["command"], "new-server");
    }

    #[tokio::test]
    async fn secret_keep_and_unknown_server_fields_survive_upsert() {
        let source = Arc::new(MemorySource::new(Some(
            r#"{"mcpServers":{"local":{"command":"old","env":{"TOKEN":"secret"},"editorField":true}}}"#,
        )));
        let store = McpConfigStore::new(source.clone());
        store
            .mutate(
                "revision-1",
                McpConfigurationMutation::Upsert {
                    server: McpServerDraft {
                        server_key: McpServerKey::new("local").expect("key"),
                        display_name: "Local".to_owned(),
                        description: "Tools".to_owned(),
                        enabled: true,
                        transport: McpServerTransportDraft::Stdio {
                            command: assistant_protocol::McpFieldChange::Replace("new".to_owned()),
                            args: assistant_protocol::McpFieldChange::Replace(Vec::new()),
                            cwd: assistant_protocol::McpFieldChange::Remove,
                            environment: BTreeMap::from([(
                                "TOKEN".to_owned(),
                                assistant_protocol::McpSecretChange::Keep,
                            )]),
                        },
                        startup_timeout_ms: None,
                        tool_timeout_ms: None,
                    },
                },
            )
            .await
            .expect("upsert");
        let guard = source.document.read().expect("source");
        let stored = &guard.as_ref().expect("document").0;
        assert!(stored.contains("secret"));
        let value: Value = serde_json::from_str(stored).expect("stored JSON");
        assert_eq!(value["mcpServers"]["local"]["editorField"], true);
        assert_eq!(value["mcpServers"]["local"]["command"], "new");
    }

    #[tokio::test]
    async fn editing_redacted_fields_keeps_values_and_explicit_removal_drops_them() {
        use assistant_protocol::McpFieldChange;
        let source = Arc::new(MemorySource::new(Some(
            r#"{"mcpServers":{"local":{"command":"server","args":["hidden-argument"],"cwd":"/tmp","env":{"TOKEN":"secret"}},"remote":{"url":"https://example.test/mcp?token=secret-query","headers":{"Authorization":"secret-header"}}}}"#,
        )));
        let store = McpConfigStore::new(source.clone());
        let snapshot = store.snapshot().await.expect("redacted snapshot");
        let encoded = serde_json::to_string(&snapshot).expect("JSON");
        assert!(!encoded.contains("hidden-argument"));
        assert!(!encoded.contains("secret-query"));
        assert!(!encoded.contains("secret-header"));
        let mut draft = McpServerDraft {
            server_key: McpServerKey::new("local").expect("key"),
            display_name: "Renamed".to_owned(),
            description: "New scope".to_owned(),
            enabled: true,
            transport: McpServerTransportDraft::Stdio {
                command: McpFieldChange::Keep,
                args: McpFieldChange::Keep,
                cwd: McpFieldChange::Keep,
                environment: BTreeMap::new(),
            },
            startup_timeout_ms: None,
            tool_timeout_ms: None,
        };
        let updated = store
            .mutate(
                &snapshot.revision,
                McpConfigurationMutation::Upsert {
                    server: draft.clone(),
                },
            )
            .await
            .expect("keep fields");
        let resolved = store
            .resolve_draft(&draft)
            .await
            .expect("test draft resolves kept fields");
        assert!(
            matches!(resolved.transport(), McpServerTransportConfig::Stdio { args, cwd, .. } if args == &["hidden-argument"] && cwd.as_deref() == Some("/tmp"))
        );
        draft.transport = McpServerTransportDraft::Stdio {
            command: McpFieldChange::Keep,
            args: McpFieldChange::Replace(Vec::new()),
            cwd: McpFieldChange::Remove,
            environment: BTreeMap::new(),
        };
        store
            .mutate(
                &updated.revision,
                McpConfigurationMutation::Upsert {
                    server: draft.clone(),
                },
            )
            .await
            .expect("remove fields");
        let resolved = store.resolve_draft(&draft).await.expect("updated draft");
        assert!(
            matches!(resolved.transport(), McpServerTransportConfig::Stdio { args, cwd, .. } if args.is_empty() && cwd.is_none())
        );
        draft.server_key = McpServerKey::new("remote").expect("key");
        draft.transport = McpServerTransportDraft::StreamableHttp {
            url: McpFieldChange::Keep,
            headers: BTreeMap::new(),
        };
        let resolved = store.resolve_draft(&draft).await.expect("keep URL query");
        assert!(
            matches!(resolved.transport(), McpServerTransportConfig::StreamableHttp { url, .. } if url.contains("secret-query"))
        );
    }

    #[tokio::test]
    async fn valid_key_with_invalid_server_value_can_be_removed_without_dropping_other_entries() {
        let source = Arc::new(MemorySource::new(Some(
            r#"{"mcpServers":{"broken":false,"healthy":{"command":"server"}}}"#,
        )));
        let store = McpConfigStore::new(source.clone());
        let before = store.snapshot().await.expect("invalid snapshot");
        assert_eq!(before.diagnostics.len(), 1);

        let after = store
            .mutate(
                "revision-1",
                McpConfigurationMutation::Remove {
                    server_key: McpServerKey::new("broken").expect("key"),
                },
            )
            .await
            .expect("remove invalid server");
        assert!(after.diagnostics.is_empty());
        assert_eq!(after.servers.len(), 1);
        assert_eq!(after.servers[0].server_key.as_str(), "healthy");

        let guard = source.document.read().expect("source");
        let value: Value =
            serde_json::from_str(&guard.as_ref().expect("document").0).expect("stored JSON");
        assert!(value["mcpServers"].get("broken").is_none());
        assert_eq!(value["mcpServers"]["healthy"]["command"], "server");
    }

    #[test]
    fn import_accepts_common_hints_but_rejects_legacy_sse() {
        let accepted = parse_import(
            r#"{"mcpServers":{"remote":{"transport":"streamable-http","url":"https://example.com/mcp"}}}"#,
        );
        assert!(accepted.diagnostics.is_empty());
        assert!(accepted.servers.values().all(Result::is_ok));

        let rejected = parse_import(
            r#"{"mcpServers":{"remote":{"transport":"sse","url":"https://example.com/mcp"}}}"#,
        );
        assert!(rejected.servers.values().all(Result::is_err));
    }

    #[test]
    fn tool_timeout_accepts_long_durations_but_rejects_unsafe_integers() {
        for milliseconds in [
            1,
            30_000,
            300_000,
            1_800_000,
            McpRuntimeConfig::MAX_TOOL_TIMEOUT_INTEGER_MS,
        ] {
            let document = serde_json::json!({"mcpServers":{"fixture":{"command":"server","toolTimeoutMs":milliseconds}}}).to_string();
            let imported = parse_import(&document);
            assert!(imported.servers.values().all(Result::is_ok));
            let parsed = parse_document(&document, None);
            let snapshot = parsed.project(&BTreeMap::new());
            assert!(snapshot.diagnostics.is_empty());
            assert_eq!(snapshot.servers.len(), 1);
            assert_eq!(snapshot.servers[0].tool_timeout_ms, Some(milliseconds));
        }
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(1_u64 << 53),
        ] {
            let document = serde_json::json!({"mcpServers":{"fixture":{"command":"server","toolTimeoutMs":invalid}}}).to_string();
            let parsed = parse_document(&document, None);
            assert!(!parsed.diagnostics.is_empty());
            assert!(parsed.servers.is_empty());
        }
    }

    #[test]
    fn snapshots_redact_secrets_and_url_queries() {
        let parsed = parse_document(
            r#"{"mcpServers":{"remote":{"url":"https://example.com/mcp?token=secret","headers":{"Authorization":"Bearer secret"}}}}"#,
            Some("revision".to_owned()),
        );
        let snapshot = parsed.project(&BTreeMap::new());
        let json = serde_json::to_string(&snapshot).expect("snapshot json");
        assert!(!json.contains("Bearer secret"));
        assert!(!json.contains("token=secret"));
        assert_eq!(snapshot.servers[0].header_keys, ["Authorization"]);
    }

    #[test]
    fn transport_conflicts_and_protocol_headers_fail_closed() {
        let conflicted = parse_document(
            r#"{"mcpServers":{"bad":{"command":"x","url":"https://example.com/mcp"}}}"#,
            None,
        );
        assert_eq!(conflicted.diagnostics.len(), 1);

        let header = parse_document(
            r#"{"mcpServers":{"bad":{"url":"https://example.com/mcp","headers":{"MCP-Session-Id":"secret"}}}}"#,
            None,
        );
        assert_eq!(header.diagnostics.len(), 1);

        let insecure_remote = parse_document(
            r#"{"mcpServers":{"bad":{"url":"http://example.com/mcp"}}}"#,
            None,
        );
        assert_eq!(insecure_remote.diagnostics.len(), 1);

        let loopback = parse_document(
            r#"{"mcpServers":{"local":{"url":"http://127.0.0.1:3000/mcp"}}}"#,
            None,
        );
        assert!(loopback.diagnostics.is_empty());
    }
}
