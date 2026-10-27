//! 包／内置定义与用户覆盖的单次归并，不持有第二份目录或配置缓存。
//!
//! 插件覆盖由 Host 提供插件 config 目录内的安全文件源；内置覆盖使用原用户 mcp.json。
//! 文件修订、包定义及原插件参数分别校验，不把不同文件／SQL 的修改包装成原子事务。

#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use assistant_protocol::{
    McpConfigurationMutation, McpConfigurationSnapshot, McpDiagnosticCode, McpDiagnosticSnapshot,
    McpManagedConfigurationSnapshot, McpSecretChange, McpServerKey, McpServerRuntimeState,
    McpServerSnapshot, McpServerSource, McpTransportKind, PluginConfigurationFieldSnapshot,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::{
    McpConfigSource, McpConfigStore, McpRegistryCandidate, McpRegistryServerProjection,
    McpServerConfig,
};
use crate::{ConfigSourceLoad, ConfigSourceReplace, RuntimeError, RuntimeResult};

/// Host 读取包后的不可变事实；raw 可能含展开的秘密，禁止 Debug／Serialize。
/// ready=false 的定义仍参与管理展示，但绝不能建立执行连接。
#[derive(Clone)]
pub struct ManagedMcpServerDefinition {
    pub source: McpServerSource,
    pub server_key: McpServerKey,
    pub definition_digest: String,
    pub source_enabled: bool,
    pub source_available: bool,
    pub ready: bool,
    pub raw: Map<String, Value>,
    pub parameter_bindings: BTreeMap<String, Vec<String>>,
    pub parameters: Vec<PluginConfigurationFieldSnapshot>,
    pub plugin_settings_revision: Option<String>,
    pub plugin_sha256: Option<String>,
    /// 插件的 config/mcp-overrides.json；Builtin 使用原 McpConfigStore 来源。
    pub configuration: Option<Arc<dyn McpConfigSource>>,
}

/// 一次文件读取的事实；只在当前命令内存在，非法记录保留原件并阻止对应服务生效。
struct OverridesDocument {
    root: Map<String, Value>,
    revision: Option<String>,
    valid: bool,
}

impl OverridesDocument {
    fn public_revision(&self) -> &str {
        self.revision.as_deref().unwrap_or("absent")
    }

    fn record(&self, definition: &ManagedMcpServerDefinition) -> RuntimeResult<Map<String, Value>> {
        if !self.valid {
            return Err(RuntimeError::McpConfigInvalid);
        }
        let section = match definition.source {
            McpServerSource::Builtin => self
                .root
                .get("managedServers")
                .map(|value| {
                    value
                        .as_object()
                        .ok_or(RuntimeError::McpConfigInvalid)?
                        .get("builtin")
                        .map(|value| value.as_object().ok_or(RuntimeError::McpConfigInvalid))
                        .transpose()
                })
                .transpose()?
                .flatten(),
            McpServerSource::Plugin(_) => self
                .root
                .get("servers")
                .map(|value| value.as_object().ok_or(RuntimeError::McpConfigInvalid))
                .transpose()?,
            McpServerSource::User => return Err(RuntimeError::McpConfigInvalid),
        };
        section
            .and_then(|section| section.get(definition.server_key.as_str()))
            .map(|value| {
                value
                    .as_object()
                    .cloned()
                    .ok_or(RuntimeError::McpConfigInvalid)
            })
            .unwrap_or_else(|| Ok(Map::new()))
    }

    fn put(
        &mut self,
        definition: &ManagedMcpServerDefinition,
        record: Map<String, Value>,
    ) -> RuntimeResult<()> {
        let section = match definition.source {
            McpServerSource::Builtin => self
                .root
                .entry("managedServers")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or(RuntimeError::McpConfigInvalid)?
                .entry("builtin")
                .or_insert_with(|| Value::Object(Map::new())),
            McpServerSource::Plugin(_) => self
                .root
                .entry("servers")
                .or_insert_with(|| Value::Object(Map::new())),
            McpServerSource::User => return Err(RuntimeError::McpConfigInvalid),
        }
        .as_object_mut()
        .ok_or(RuntimeError::McpConfigInvalid)?;
        if record.is_empty() {
            section.remove(definition.server_key.as_str());
        } else {
            section.insert(definition.server_key.to_string(), Value::Object(record));
        }
        Ok(())
    }
}

async fn load(source: &dyn McpConfigSource, builtin: bool) -> RuntimeResult<OverridesDocument> {
    let (contents, revision) = match source.load().await {
        ConfigSourceLoad::Missing => {
            return Ok(OverridesDocument {
                root: if builtin {
                    Map::from_iter([("mcpServers".to_owned(), Value::Object(Map::new()))])
                } else {
                    Map::new()
                },
                revision: None,
                valid: true,
            });
        }
        ConfigSourceLoad::Document(document) => (
            document.contents().to_owned(),
            Some(document.revision().to_owned()),
        ),
        ConfigSourceLoad::Unavailable(_) => return Err(RuntimeError::ConfigurationUnavailable),
    };
    let root = serde_json::from_str::<Value>(&contents)
        .ok()
        .and_then(|value| value.as_object().cloned());
    let valid = root.as_ref().is_some_and(|root| {
        if builtin {
            root.get("mcpServers").is_some_and(Value::is_object)
        } else {
            root.get("servers").is_some_and(Value::is_object)
        }
    });
    Ok(OverridesDocument {
        root: root.unwrap_or_default(),
        revision,
        valid,
    })
}

impl McpConfigStore {
    fn managed_source<'a>(
        &'a self,
        definition: &'a ManagedMcpServerDefinition,
    ) -> RuntimeResult<&'a dyn McpConfigSource> {
        match definition.source {
            McpServerSource::Builtin => Ok(self.source.as_ref()),
            McpServerSource::Plugin(_) => definition
                .configuration
                .as_deref()
                .ok_or(RuntimeError::PluginPackageUnavailable),
            McpServerSource::User => Err(RuntimeError::McpConfigInvalid),
        }
    }

    /// 调用方已冻结包定义；文件在短写门内读取，不进行握手／网络等待。
    /// 返回管理投影和执行候选来自同一次文件读取；非法单条覆盖不拖垮其他来源。
    pub(crate) async fn resolve_managed(
        &self,
        definitions: &[ManagedMcpServerDefinition],
        sources: &[(McpServerSource, Arc<dyn McpConfigSource>)],
        runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
    ) -> RuntimeResult<(McpConfigurationSnapshot, McpRegistryCandidate)> {
        let _gate = self.gate.lock().await;
        self.resolve_managed_locked(definitions, sources, runtime)
            .await
    }

    /// 调用方已持文件门；一次命令内每个来源只读一次，返回投影和候选共用这些事实。
    /// 只准备配置，不建立连接；刷新调用方还须持来源短租约以覆盖提交前复核。
    pub(crate) async fn resolve_managed_locked(
        &self,
        definitions: &[ManagedMcpServerDefinition],
        sources: &[(McpServerSource, Arc<dyn McpConfigSource>)],
        runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
    ) -> RuntimeResult<(McpConfigurationSnapshot, McpRegistryCandidate)> {
        let loaded = self.load().await?;
        let mut candidate = loaded.candidate();
        let builtin_document = OverridesDocument {
            root: loaded.root.clone(),
            revision: loaded.revision.clone(),
            valid: loaded.mutable,
        };
        let mut documents = BTreeMap::from([(McpServerSource::Builtin, Some(builtin_document))]);
        for (source, file) in sources {
            if matches!(source, McpServerSource::Plugin(_)) {
                documents.insert(source.clone(), load(file.as_ref(), false).await.ok());
            }
        }
        // 用户文件中损坏的显式配置仍占据用户来源，不静默改用另一份凭据或启动命令。
        let mut selected: BTreeMap<_, _> = candidate
            .configured_keys
            .iter()
            .filter(|key| {
                !candidate.servers.contains_key(*key)
                    && loaded
                        .root
                        .get("mcpServers")
                        .and_then(|servers| servers.get(key.as_str()))
                        .and_then(|server| server.get("enabled"))
                        .and_then(Value::as_bool)
                        != Some(false)
            })
            .map(|key| (key.clone(), McpServerSource::User))
            .collect();
        let mut snapshot = loaded.project(runtime);
        let mut groups: BTreeMap<_, Vec<_>> = std::mem::take(&mut candidate.servers)
            .into_iter()
            .map(|(key, server)| (key, vec![server]))
            .collect();
        let mut defined = candidate.configured_keys.clone();
        for definition in definitions {
            if !documents.contains_key(&definition.source) {
                let document = match self.managed_source(definition) {
                    Ok(source) => load(source, false).await.ok(),
                    Err(_) => None,
                };
                documents.insert(definition.source.clone(), document);
            }
            let document = documents.get(&definition.source).and_then(Option::as_ref);
            let revision = document
                .map_or("unavailable", OverridesDocument::public_revision)
                .to_owned();
            let record = document
                .ok_or(RuntimeError::McpConfigInvalid)
                .and_then(|document| document.record(definition));
            let resolved = record
                .as_ref()
                .map_err(|_| RuntimeError::McpConfigInvalid)
                .and_then(|record| apply(definition, record));
            let server_snapshot = project(
                definition,
                revision,
                record.as_ref().ok(),
                &resolved,
                runtime,
            );
            defined.insert(definition.server_key.clone());
            if let Ok(server) = resolved {
                groups
                    .entry(definition.server_key.clone())
                    .or_default()
                    .push(server);
            } else {
                snapshot.diagnostics.push(invalid(definition));
            }
            snapshot.servers.push(server_snapshot);
        }
        // 来源的 Ord 固定为 User > Plugin（标识升序）> Builtin，不依赖发现/安装顺序。
        for server in snapshot.servers.iter().filter(|server| server.enabled) {
            selected
                .entry(server.server_key.clone())
                .and_modify(|source| {
                    if server.source < *source {
                        *source = server.source.clone();
                    }
                })
                .or_insert_with(|| server.source.clone());
        }
        for key in &defined {
            let mut servers = groups.remove(key).unwrap_or_default();
            servers.sort_by(|left, right| left.source.cmp(&right.source));
            let winner = selected.get(key);
            let chosen = match winner {
                Some(source) => servers.into_iter().find(|server| &server.source == source),
                None => servers.into_iter().next(),
            };
            if let Some(server) = chosen {
                candidate.servers.insert(key.clone(), server);
            } else if winner.is_some() {
                candidate.blocked_keys.insert(key.clone());
            }
        }
        for server in &mut snapshot.servers {
            server.shadowed_by = selected
                .get(&server.server_key)
                .filter(|source| server.enabled && *source != &server.source)
                .cloned();
            if server.shadowed_by.is_some()
                || !server.enabled
                || candidate.blocked_keys.contains(&server.server_key)
            {
                // 被覆盖的定义不索要连接，也不能冒用同名胜出来源的连接或待刷新状态。
                // 若自己仍是旧活动来源，保持待刷新，直到原 Registry 完成退役。
                server.needs_refresh = runtime
                    .get(&server.server_key)
                    .is_some_and(|state| state.source == server.source);
            }
        }
        // 包升级移除最后一项服务时也读取同一个原文件，仅诊断并保留覆盖，不自动删 Key。
        for (source, document) in &documents {
            let Some(document) = document else { continue };
            let section = match source {
                McpServerSource::Builtin => document
                    .root
                    .get("managedServers")
                    .and_then(|value| value.get("builtin")),
                McpServerSource::Plugin(_) => document.root.get("servers"),
                McpServerSource::User => None,
            };
            if let Some(records) = section.and_then(Value::as_object) {
                for key in records.keys() {
                    if definitions.iter().any(|definition| {
                        &definition.source == source && definition.server_key.as_str() == key
                    }) {
                        continue;
                    }
                    snapshot.diagnostics.push(McpDiagnosticSnapshot {
                        server_key:McpServerKey::new(key.clone()).ok(), code:McpDiagnosticCode::InvalidConfig, field_path:Some("servers".to_owned()),
                        message: match source {
                            McpServerSource::Plugin(name) => format!("来源：插件 · {name}；保存的 MCP 配置没有对应的服务声明，配置值已保留"),
                            _ => "来源：内置资源；保存的 MCP 配置没有对应的服务声明，配置值已保留".to_owned(),
                        },
                    });
                }
            }
        }
        candidate.configured_keys = defined;
        candidate.diagnostics.extend(snapshot.diagnostics.clone());
        snapshot.needs_refresh = runtime
            .keys()
            .any(|key| !candidate.configured_keys.contains(key))
            || snapshot.servers.iter().any(|server| server.needs_refresh);
        Ok((snapshot, candidate))
    }

    /// 已取得来源租约后才进入文件门；可靠替换后不因晚取消撤销，且不在此刷新连接。
    #[cfg(test)]
    pub(crate) async fn mutate_managed(
        &self,
        expected_revision: &str,
        mutation: &McpConfigurationMutation,
        definition: &ManagedMcpServerDefinition,
    ) -> RuntimeResult<()> {
        let _gate = self.gate.lock().await;
        self.mutate_managed_locked(expected_revision, mutation, definition)
            .await
    }

    /// 调用方拥有文件写门与来源租约，且以 RuntimeTasks 保持提交任务所有权。
    /// 替换成功后不因请求 future 被丢弃撤销；此函数不刷新连接或写插件参数。
    pub(crate) async fn mutate_managed_locked(
        &self,
        expected_revision: &str,
        mutation: &McpConfigurationMutation,
        definition: &ManagedMcpServerDefinition,
    ) -> RuntimeResult<()> {
        let source = self.managed_source(definition)?;
        let mut document = load(
            source,
            matches!(definition.source, McpServerSource::Builtin),
        )
        .await?;
        if document.public_revision() != expected_revision {
            return Err(RuntimeError::McpConfigConflict);
        }
        let mut record = document.record(definition)?;
        match mutation {
            McpConfigurationMutation::PatchManaged {
                enabled,
                environment,
                headers,
                ..
            } => {
                validate_intent_keys(definition, environment.keys(), headers.keys())?;
                if let Some(enabled) = enabled {
                    record.insert("enabled".to_owned(), Value::Bool(*enabled));
                }
                patch_map(&mut record, "env", environment)?;
                patch_map(&mut record, "headers", headers)?;
            }
            McpConfigurationMutation::ResetManaged {
                enabled,
                environment,
                headers,
                ..
            } => {
                validate_intent_keys(definition, environment.iter(), headers.iter())?;
                if *enabled {
                    record.remove("enabled");
                }
                reset_map(&mut record, "env", environment)?;
                reset_map(&mut record, "headers", headers)?;
            }
            _ => return Err(RuntimeError::McpConfigInvalid),
        }
        // 即使缺参数尚不能执行，也必须完整校验变量、类型、绑定与保留键。
        validate_record(definition, &record)?;
        if definition.ready {
            apply(definition, &record)?;
        }
        let before = document.root.clone();
        document.put(definition, record)?;
        if before == document.root {
            return Ok(());
        }
        let contents = serde_json::to_string_pretty(&document.root)
            .map_err(|_| RuntimeError::McpConfigInvalid)?
            + "\n";
        match source.replace(document.revision, contents).await {
            ConfigSourceReplace::Applied(_) => Ok(()),
            ConfigSourceReplace::Conflict(_) => Err(RuntimeError::McpConfigConflict),
            ConfigSourceReplace::Unavailable(_) => {
                Err(RuntimeError::ConfigurationPersistenceFailed)
            }
        }
    }

    /// 复用已保存覆盖解析临时草稿，只读实际文件并核验内容修订，不写入或刷新活动连接。
    /// 调用方负责包来源租约及测试完成后的来源复验；文件门不跨握手或目录请求。
    pub(crate) async fn managed_test_config(
        &self,
        draft: &assistant_protocol::McpManagedServerTestDraft,
        definition: &ManagedMcpServerDefinition,
    ) -> RuntimeResult<McpServerConfig> {
        let _gate = self.gate.lock().await;
        let document = load(
            self.managed_source(definition)?,
            matches!(definition.source, McpServerSource::Builtin),
        )
        .await?;
        if document.public_revision() != draft.expected_revision {
            return Err(RuntimeError::McpConfigConflict);
        }
        let mut record = document.record(definition)?;
        validate_intent_keys(definition, draft.environment.keys(), draft.headers.keys())?;
        patch_map(&mut record, "env", &draft.environment)?;
        patch_map(&mut record, "headers", &draft.headers)?;
        apply(definition, &record)
    }
}

fn patch_map(
    record: &mut Map<String, Value>,
    field: &str,
    changes: &BTreeMap<String, McpSecretChange>,
) -> RuntimeResult<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let values = record
        .entry(field)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(RuntimeError::McpConfigInvalid)?;
    for (key, change) in changes {
        match change {
            McpSecretChange::Keep => {}
            McpSecretChange::Replace(value) => {
                values.insert(key.clone(), Value::String(value.expose().to_owned()));
            }
            McpSecretChange::Remove => {
                values.insert(key.clone(), Value::Null);
            }
        }
    }
    Ok(())
}

fn reset_map(record: &mut Map<String, Value>, field: &str, keys: &[String]) -> RuntimeResult<()> {
    if let Some(values) = record.get_mut(field) {
        let values = values
            .as_object_mut()
            .ok_or(RuntimeError::McpConfigInvalid)?;
        for key in keys {
            values.remove(key);
        }
        if values.is_empty() {
            record.remove(field);
        }
    }
    Ok(())
}

fn reserved(key: &str) -> bool {
    [
        "EZ_ASSISTANT_PLUGIN_ROOT",
        "EZ_ASSISTANT_PLUGIN_CONFIG_DIR",
        "EZ_ASSISTANT_PLUGIN_DATA_DIR",
    ]
    .iter()
    .any(|name| key.eq_ignore_ascii_case(name))
}

fn validate_record(
    definition: &ManagedMcpServerDefinition,
    record: &Map<String, Value>,
) -> RuntimeResult<()> {
    if !definition.source_available
        || record
            .keys()
            .any(|key| !matches!(key.as_str(), "enabled" | "env" | "headers"))
        || record
            .get("enabled")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err(RuntimeError::McpConfigInvalid);
    }
    let stdio = definition.raw.contains_key("command");
    for (field, env) in [("env", true), ("headers", false)] {
        let Some(values) = record.get(field) else {
            continue;
        };
        let values = values.as_object().ok_or(RuntimeError::McpConfigInvalid)?;
        if (!values.is_empty() && env != stdio) || values.len() > 64 {
            return Err(RuntimeError::McpConfigInvalid);
        }
        let mut normalized = Map::new();
        for (key, value) in values {
            if reserved(key)
                || definition.parameter_bindings.keys().any(|bound| {
                    if env {
                        bound == key
                    } else {
                        bound.eq_ignore_ascii_case(key)
                    }
                })
            {
                return Err(RuntimeError::McpConfigInvalid);
            }
            if !value.is_null() && !value.is_string() {
                return Err(RuntimeError::McpConfigInvalid);
            }
            normalized.insert(
                key.clone(),
                if value.is_null() {
                    Value::String(String::new())
                } else {
                    value.clone()
                },
            );
        }
        super::config::secret_map(
            &Map::from_iter([(field.to_owned(), Value::Object(normalized))]),
            field,
            env,
            &definition.server_key,
        )
        .map_err(|_| RuntimeError::McpConfigInvalid)?;
    }
    Ok(())
}

fn apply(
    definition: &ManagedMcpServerDefinition,
    record: &Map<String, Value>,
) -> RuntimeResult<McpServerConfig> {
    validate_record(definition, record)?;
    if !definition.ready {
        return Err(RuntimeError::McpConfigInvalid);
    }
    let mut raw = definition.raw.clone();
    let mut removed = BTreeSet::new();
    if let Some(value) = record.get("enabled") {
        raw.insert("enabled".to_owned(), value.clone());
    }
    if !definition.source_enabled {
        raw.insert("enabled".to_owned(), Value::Bool(false));
    }
    for field in ["env", "headers"] {
        if let Some(values) = record.get(field).and_then(Value::as_object) {
            let effective = raw
                .entry(field)
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or(RuntimeError::McpConfigInvalid)?;
            for (key, value) in values {
                if field == "headers" || cfg!(windows) && field == "env" {
                    effective.retain(|name, _| !name.eq_ignore_ascii_case(key));
                }
                if value.is_null() {
                    effective.remove(key);
                    if field == "env" {
                        removed.insert(key.clone());
                    }
                } else {
                    effective.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let mut config = super::config::normalize_server(&definition.server_key, raw)
        .map_err(|_| RuntimeError::McpConfigInvalid)?
        .config;
    if let McpServerSource::Plugin(name) = &definition.source {
        config.plugin_name = Some(name.clone());
    }
    config.source = definition.source.clone();
    config.plugin_sha256 = definition.plugin_sha256.clone();
    // 包换代、参数变化和显式移除也参与连接身份；不能复用同名旧连接投影。
    let mut hash = Sha256::new();
    hash.update(config.fingerprint);
    hash.update(definition.definition_digest.as_bytes());
    hash.update(
        definition
            .plugin_settings_revision
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    for name in &removed {
        hash.update(name.as_bytes());
        hash.update([0]);
    }
    config.fingerprint = hash.finalize().into();
    config.removed_environment = removed;
    Ok(config)
}

fn project(
    definition: &ManagedMcpServerDefinition,
    revision: String,
    record: Option<&Map<String, Value>>,
    resolved: &RuntimeResult<McpServerConfig>,
    runtime: &BTreeMap<McpServerKey, McpRegistryServerProjection>,
) -> McpServerSnapshot {
    let ready = resolved.is_ok();
    let mut snapshot = if let Ok(config) = resolved {
        super::config::project_server(config.clone(), runtime)
    } else {
        McpServerSnapshot {
            source: definition.source.clone(),
            managed: None,
            shadowed_by: None,
            server_key: definition.server_key.clone(),
            display_name: definition.server_key.to_string(),
            description: String::new(),
            transport: if definition.raw.contains_key("url") {
                McpTransportKind::StreamableHttp
            } else {
                McpTransportKind::Stdio
            },
            enabled: definition.source_enabled
                && definition
                    .raw
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            runtime_state: McpServerRuntimeState::Unavailable,
            tool_count: 0,
            needs_refresh: false,
            target_summary: String::new(),
            startup_timeout_ms: None,
            tool_timeout_ms: None,
            environment_keys: Vec::new(),
            header_keys: Vec::new(),
        }
    };
    snapshot.source = definition.source.clone();
    let configured_enabled = record
        .and_then(|record| record.get("enabled"))
        .or_else(|| definition.raw.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    snapshot.enabled = definition.source_enabled && configured_enabled;
    if let Some(applied) = runtime
        .get(&definition.server_key)
        .filter(|state| state.source == definition.source)
    {
        snapshot.runtime_state = applied.state;
        snapshot.tool_count = applied.tool_count;
    } else if !snapshot.enabled {
        snapshot.runtime_state = McpServerRuntimeState::Disabled;
    }
    // 展开的 command／URL 可能含参数秘密；受管管理界面只公开类型与归属。
    snapshot.target_summary = match snapshot.transport {
        McpTransportKind::Stdio => "Managed stdio",
        McpTransportKind::StreamableHttp => "Managed HTTP",
    }
    .to_owned();
    let states = |field: &str| -> BTreeMap<String, bool> {
        let mut result: BTreeMap<String, bool> = definition
            .raw
            .get(field)
            .and_then(Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            value.as_str().is_some_and(|value| !value.is_empty()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(config) = resolved {
            let values = match (&config.transport, field) {
                (super::McpServerTransportConfig::Stdio { environment, .. }, "env") => {
                    Some(environment)
                }
                (super::McpServerTransportConfig::StreamableHttp { headers, .. }, "headers") => {
                    Some(headers)
                }
                _ => None,
            };
            if let Some(values) = values {
                result = values
                    .iter()
                    .map(|(key, value)| (key.clone(), !value.expose().is_empty()))
                    .collect();
            }
        }
        if let Some(values) = record
            .and_then(|record| record.get(field))
            .and_then(Value::as_object)
        {
            for (key, value) in values {
                result.insert(
                    key.clone(),
                    value.as_str().is_some_and(|value| !value.is_empty()),
                );
            }
        }
        result
    };
    let environment = states("env");
    let headers = states("headers");
    snapshot.environment_keys = environment.keys().cloned().collect();
    snapshot.header_keys = headers.keys().cloned().collect();
    snapshot.managed = Some(McpManagedConfigurationSnapshot {
        revision,
        definition_digest: definition.definition_digest.clone(),
        configured_enabled,
        source_enabled: definition.source_enabled,
        source_available: definition.source_available,
        configuration_ready: ready,
        environment,
        headers,
        parameter_bindings: definition.parameter_bindings.clone(),
        parameters: definition.parameters.clone(),
        plugin_settings_revision: definition.plugin_settings_revision.clone(),
        plugin_sha256: definition.plugin_sha256.clone(),
    });
    snapshot
}

fn invalid(definition: &ManagedMcpServerDefinition) -> McpDiagnosticSnapshot {
    let source = match &definition.source {
        McpServerSource::User => "用户添加".to_owned(),
        McpServerSource::Plugin(name) => format!("插件 · {name}"),
        McpServerSource::Builtin => "内置资源".to_owned(),
    };
    let missing = definition
        .parameters
        .iter()
        .filter(|field| field.required && !field.is_set)
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    let reason = if !definition.source_available {
        "服务来源不可用，请检查插件是否已安装且包文件完整".to_owned()
    } else if !missing.is_empty() {
        format!(
            "缺少必填插件配置项：{}。请在插件配置中填写后刷新此服务",
            missing.join("、")
        )
    } else {
        "MCP 服务定义或覆盖配置无效，请检查该来源的配置".to_owned()
    };
    McpDiagnosticSnapshot {
        server_key: Some(definition.server_key.clone()),
        code: McpDiagnosticCode::InvalidConfig,
        field_path: None,
        message: format!("来源：{source}；{reason}"),
    }
}

fn validate_intent_keys<'a>(
    definition: &ManagedMcpServerDefinition,
    environment: impl Iterator<Item = &'a String>,
    headers: impl Iterator<Item = &'a String>,
) -> RuntimeResult<()> {
    let record = Map::from_iter([
        (
            "env".to_owned(),
            Value::Object(environment.map(|key| (key.clone(), Value::Null)).collect()),
        ),
        (
            "headers".to_owned(),
            Value::Object(headers.map(|key| (key.clone(), Value::Null)).collect()),
        ),
    ]);
    validate_record(definition, &record)
}
