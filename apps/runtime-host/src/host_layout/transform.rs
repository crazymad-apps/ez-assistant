//! 布局升级中定义过的配置字段转换；不重写脚本、环境变量或历史对话。

use super::{Error, Result, paths::Relocation};
use std::path::Path;
use toml_edit::{DocumentMut, Item};

pub(super) fn configurations(
    original: Option<&str>,
    paths: &Relocation,
) -> Result<(String, Option<String>)> {
    if let Some(original) = original {
        crate::access::validate_layout_configuration(original).map_err(|_| Error::Configuration)?;
    }
    let mut host = crate::host_configuration::personal_document();
    let personal = original
        .map(|original| -> Result<String> {
            let mut document = original
                .parse::<DocumentMut>()
                .map_err(|_| Error::Configuration)?;
            if let Some(mut access) = document.remove("host_access") {
                for field in ["tls_certificate", "tls_private_key"] {
                    if let Some(item) = access.get_mut(field)
                        && let Some(path) = item.as_str()
                    {
                        *item = toml_edit::value(paths.text(path));
                    }
                }
                host["host_access"] = access;
            }
            Ok(document.to_string())
        })
        .transpose()?;
    if host.get("host_access").is_none() {
        let access = toml::to_string(&assistant_protocol::HostAccessConfiguration::default())
            .map_err(|_| Error::Configuration)?
            .parse::<DocumentMut>()
            .map_err(|_| Error::Configuration)?;
        host["host_access"] = Item::Table(access.as_table().clone());
    }
    Ok((host.to_string(), personal))
}

pub(super) fn file(
    relative: &Path,
    original: &[u8],
    paths: &Relocation,
) -> Result<Option<Vec<u8>>> {
    let mcp = relative == Path::new("mcp.json");
    if !is_configuration(relative) {
        return Ok(None);
    }
    if !mcp && assistant_runtime::PermissionDocument::parse(original).is_err() {
        return Ok(None);
    }
    // 已有损坏文档保留原件供普通配置诊断，不能替换成默认值。
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(original) else {
        return Ok(None);
    };
    let before = json.clone();
    if mcp {
        if let Some(servers) = json.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
            for server in servers.values_mut() {
                relocate_field(server, "cwd", paths);
            }
        }
    } else if let Some(rules) = json.get_mut("rules").and_then(|v| v.as_array_mut()) {
        for rule in rules {
            if let Some(matcher) = rule.get_mut("matcher") {
                match matcher.get("type").and_then(|v| v.as_str()) {
                    Some("file") => relocate_field(matcher, "path", paths),
                    Some("shell") => relocate_field(matcher, "working_directory", paths),
                    _ => {}
                }
            }
        }
    }
    if json == before {
        return Ok(None);
    }
    let mut bytes = serde_json::to_vec_pretty(&json)?;
    bytes.push(b'\n');
    Ok(Some(bytes))
}

pub(super) fn is_configuration(relative: &Path) -> bool {
    let components = relative.iter().collect::<Vec<_>>();
    let permission = relative == Path::new("permissions.json")
        || (components.len() == 5
            && components[0] == "data"
            && components[1] == "sessions"
            && components[3] == "private"
            && components[4] == "permissions.json")
        || (components.len() == 5
            && components[0] == "data"
            && components[1] == "workspaces"
            && components[3] == "agent"
            && components[4] == "permissions.json");
    let mcp = relative == Path::new("mcp.json");
    permission || mcp
}

fn relocate_field(value: &mut serde_json::Value, field: &str, paths: &Relocation) {
    if let Some(path) = value.get(field).and_then(|v| v.as_str()) {
        let mapped = paths.text(path);
        value[field] = serde_json::Value::String(mapped);
    }
}
