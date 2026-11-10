//! 中心与 Host 的包入口契约在共享夹具上对齐；此处不加载页面或执行插件。

use std::collections::BTreeSet;

use assistant_protocol::{PluginUi, PluginUiEntryKind, PluginUiResource};

use crate::{RuntimeError, RuntimeResult};

/// 在包扫描的阻塞边界检查完整 UI 声明；不访问文件、网络或数据库。
///
/// # Errors
/// 字段超限、用途冲突、资源越界或离线 JSON Schema 无效时返回 InvalidRequest。
pub fn validate_plugin_ui(ui: &PluginUi, servers: &[String]) -> RuntimeResult<()> {
    let invalid = || RuntimeError::InvalidRequest {
        reason: "插件 UI 入口声明无效",
    };
    if ui.entries.is_empty() || ui.entries.len() > 128 {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for entry in &ui.entries {
        if !identifier(&entry.id)
            || !ids.insert(&entry.id)
            || entry.title.trim().is_empty()
            || entry.title.chars().count() > 128
            || entry.title.chars().any(char::is_control)
            || entry
                .icon
                .as_deref()
                .is_some_and(|path| !resource_path(path))
            || !entry.input_schema.is_object()
            || entry
                .input_schema
                .get("$schema")
                .is_some_and(|v| v.as_str() != Some("https://json-schema.org/draft/2020-12/schema"))
            || crate::mcp::compile_schema(&entry.input_schema).is_err()
            || entry.capabilities.len() > 5
            || entry
                .capabilities
                .iter()
                .enumerate()
                .any(|(i, item)| entry.capabilities[..i].contains(item))
            || entry.readable_snapshot_schemas.len() > 32
            || entry
                .readable_snapshot_schemas
                .iter()
                .any(|s| !identifier(s))
            || entry
                .readable_snapshot_schemas
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != entry.readable_snapshot_schemas.len()
        {
            return Err(invalid());
        }
        match &entry.resource {
            PluginUiResource::Package { path }
                if resource_path(path) && path.ends_with(".html") => {}
            PluginUiResource::Mcp { server, uri }
                if servers.contains(server)
                    && uri.starts_with("ui://")
                    && uri.len() <= 1024
                    && !uri.chars().any(char::is_control) => {}
            _ => return Err(invalid()),
        }
        match entry.kind {
            PluginUiEntryKind::Card if entry.snapshot_schema.as_deref().is_some_and(identifier) => {
            }
            PluginUiEntryKind::App
                if entry.snapshot_schema.is_none()
                    && entry.readable_snapshot_schemas.is_empty() => {}
            _ => return Err(invalid()),
        }
        for (domains, connection) in [
            (&entry.csp.connect_domains, true),
            (&entry.csp.resource_domains, false),
            (&entry.csp.frame_domains, false),
            (&entry.csp.base_uri_domains, false),
        ] {
            if domains.len() > 32 || domains.iter().any(|domain| !origin(domain, connection)) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn resource_path(value: &str) -> bool {
    value.starts_with("resources/")
        && value.len() <= 1024
        && !value.contains(['\\', '\0'])
        && !value.chars().any(char::is_control)
        && !value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn origin(value: &str, connection: bool) -> bool {
    if value.len() > 2048 || !value.is_ascii() || value.contains(['\'', '"', ';', '*']) {
        return false;
    }
    url::Url::parse(value).is_ok_and(|url| {
        (url.scheme() == "https" || connection && url.scheme() == "wss")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && value == url.origin().ascii_serialization()
    })
}
