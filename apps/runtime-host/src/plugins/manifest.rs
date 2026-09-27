//! 与 C01 format_version=1 对齐的清单及入口静态准入；此阶段不启动包内程序。

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use semver::Version;
use serde::Deserialize;
use serde_json::Value;

use super::{
    PackageError, Result,
    archive::{EntryIndex, EntryKind},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Platform {
    pub(crate) os: String,
    pub(crate) arch: String,
}

impl Platform {
    fn key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Configuration {
    pub(crate) name: String,
    label: String,
    pub(crate) required: bool,
    secret: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalSoftware {
    pub(crate) name: String,
    display_name: String,
    instructions: String,
    download_url: Option<String>,
    platforms: Option<Vec<Platform>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginManifest {
    format_version: u32,
    pub(crate) name: String,
    display_name: String,
    description: String,
    pub(crate) version: String,
    pub(crate) platforms: Vec<Platform>,
    min_host_version: String,
    pub(crate) skills: Vec<String>,
    pub(crate) mcp: Vec<String>,
    pub(crate) configuration: Vec<Configuration>,
    pub(crate) external_software: Vec<ExternalSoftware>,
}

fn valid_text(value: &str, max: usize, allow_empty: bool, multiline: bool) -> bool {
    (allow_empty || !value.trim().is_empty())
        && value.chars().count() <= max
        && value.chars().all(|ch| {
            let code = ch as u32;
            code != 127 && (code >= 32 || (multiline && matches!(ch, '\n' | '\r' | '\t')))
        })
}

pub(crate) fn plugin_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn server_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(b))
}

fn version(value: &str) -> bool {
    value.len() <= 80
        && value.split('.').count() == 3
        && value.split('.').all(|part| {
            !part.is_empty()
                && (part == "0" || !part.starts_with('0'))
                && part.bytes().all(|b| b.is_ascii_digit())
                && part
                    .parse::<u64>()
                    .is_ok_and(|value| value <= 9_007_199_254_740_991)
        })
        && Version::parse(value).is_ok()
}

fn unique<'a>(items: impl IntoIterator<Item = &'a String>) -> bool {
    let mut seen = BTreeSet::new();
    items.into_iter().all(|item| seen.insert(item))
}

impl PluginManifest {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 1024 * 1024 {
            return Err(PackageError::TooLarge);
        }
        let manifest: Self = serde_json::from_slice(bytes).map_err(|_| PackageError::Invalid)?;
        if manifest.format_version != 1
            || !valid_text(&manifest.name, 64, false, false)
            || !plugin_name(&manifest.name)
            || !valid_text(&manifest.display_name, 128, false, false)
            || !valid_text(&manifest.description, 4096, true, true)
            || !version(&manifest.version)
            || !version(&manifest.min_host_version)
            || manifest.platforms.is_empty()
            || manifest.platforms.len() > 6
            || !manifest.platforms.iter().all(|p| {
                matches!(p.os.as_str(), "macos" | "windows" | "linux")
                    && matches!(p.arch.as_str(), "x64" | "arm64")
            })
            || !unique(
                manifest
                    .platforms
                    .iter()
                    .map(Platform::key)
                    .collect::<Vec<_>>()
                    .iter(),
            )
            || manifest.skills.len() > 256
            || manifest.mcp.len() > 256
            || manifest.skills.is_empty() && manifest.mcp.is_empty()
            || !unique(&manifest.skills)
            || !unique(&manifest.mcp)
            || !manifest
                .skills
                .iter()
                .all(|name| plugin_name(name) && name.len() <= 64)
            || !manifest.mcp.iter().all(|name| server_name(name))
            || manifest.configuration.len() > 128
            || manifest.external_software.len() > 128
            || !unique(manifest.configuration.iter().map(|c| &c.name))
            || !unique(manifest.external_software.iter().map(|s| &s.name))
        {
            return Err(PackageError::Invalid);
        }
        for field in &manifest.configuration {
            if !server_name(&field.name) || !valid_text(&field.label, 128, false, false) {
                return Err(PackageError::Invalid);
            }
            let _ = (field.required, field.secret);
        }
        for software in &manifest.external_software {
            if !plugin_name(&software.name)
                || software.name.len() > 64
                || !valid_text(&software.display_name, 128, false, false)
                || !valid_text(&software.instructions, 4096, true, true)
                || software.download_url.as_ref().is_some_and(|url| {
                    !valid_text(url, 2048, false, false)
                        || !url::Url::parse(url)
                            .is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
                })
                || software.platforms.as_ref().is_some_and(|scope| {
                    scope.is_empty()
                        || scope.len() > 6
                        || !unique(scope.iter().map(Platform::key).collect::<Vec<_>>().iter())
                        || !scope.iter().all(|p| {
                            manifest
                                .platforms
                                .iter()
                                .any(|declared| declared.os == p.os && declared.arch == p.arch)
                        })
                })
            {
                return Err(PackageError::Invalid);
            }
        }
        Ok(manifest)
    }

    pub(super) fn supports_host(&self) -> bool {
        let Ok(required) = Version::parse(&self.min_host_version) else {
            return false;
        };
        Version::parse(env!("CARGO_PKG_VERSION")).is_ok_and(|current| current >= required)
    }

    /// 完整索引建立后统一检查声明与实际入口；任何平台缺项均拒绝整个包。
    pub(super) fn validate_entries(&self, root: &Path, index: &EntryIndex) -> Result<()> {
        let platforms: BTreeSet<_> = self.platforms.iter().map(Platform::key).collect();
        let windows = self.platforms.iter().any(|p| p.os == "windows");
        for (path, entry) in index {
            let parts: Vec<_> = path.split('/').collect();
            if windows
                && (entry.kind == EntryKind::Link
                    || parts.iter().any(|part| {
                        part.ends_with(['.', ' '])
                            || part
                                .chars()
                                .any(|ch| ['<', '>', ':', '"', '|', '?', '*'].contains(&ch))
                            || matches!(
                                part.split('.')
                                    .next()
                                    .unwrap_or("")
                                    .to_ascii_lowercase()
                                    .as_str(),
                                "con"
                                    | "prn"
                                    | "aux"
                                    | "nul"
                                    | "com1"
                                    | "com2"
                                    | "com3"
                                    | "com4"
                                    | "com5"
                                    | "com6"
                                    | "com7"
                                    | "com8"
                                    | "com9"
                                    | "lpt1"
                                    | "lpt2"
                                    | "lpt3"
                                    | "lpt4"
                                    | "lpt5"
                                    | "lpt6"
                                    | "lpt7"
                                    | "lpt8"
                                    | "lpt9"
                            )
                    }))
            {
                return Err(PackageError::Invalid);
            }
            if parts.len() > 1
                && match parts[0] {
                    "skills" => !self.skills.iter().any(|name| name == parts[1]),
                    "mcp" => !self.mcp.iter().any(|name| name == parts[1]),
                    "runtimes" => !platforms.contains(parts[1]),
                    _ => false,
                }
            {
                return Err(PackageError::Invalid);
            }
        }
        for name in &self.skills {
            let path = format!("skills/{name}/SKILL.md");
            let bytes = read_descriptor(root, index, &path, 256 * 1024)?;
            validate_skill(&bytes, name)?;
        }
        for name in &self.mcp {
            let common = format!("mcp/{name}/server.json");
            if index.contains_key(&common) {
                validate_mcp(
                    &read_descriptor(root, index, &common, 256 * 1024)?,
                    name,
                    &self.platforms[0],
                    self,
                    index,
                )?;
            }
            let prefix = format!("mcp/{name}/server.");
            for path in index.keys() {
                if let Some(variant) = path
                    .strip_prefix(&prefix)
                    .and_then(|value| value.strip_suffix(".json"))
                {
                    let recognized = ["macos", "windows", "linux"].iter().any(|os| {
                        ["x64", "arm64"]
                            .iter()
                            .any(|arch| variant == format!("{os}-{arch}"))
                    });
                    if recognized && !platforms.contains(variant) {
                        return Err(PackageError::Invalid);
                    }
                }
            }
            for platform in &self.platforms {
                let scoped = format!("mcp/{name}/server.{}.json", platform.key());
                if index.contains_key(&scoped) {
                    validate_mcp(
                        &read_descriptor(root, index, &scoped, 256 * 1024)?,
                        name,
                        platform,
                        self,
                        index,
                    )?;
                } else if !index.contains_key(&common) {
                    return Err(PackageError::Invalid);
                }
            }
        }
        Ok(())
    }
}

fn read_descriptor(root: &Path, index: &EntryIndex, path: &str, limit: usize) -> Result<Vec<u8>> {
    let entry = index.get(path).ok_or(PackageError::Invalid)?;
    if entry.kind != EntryKind::File || entry.size > limit as u64 {
        return Err(PackageError::Invalid);
    }
    std::fs::read(root.join(path)).map_err(Into::into)
}

fn validate_skill(bytes: &[u8], expected: &str) -> Result<()> {
    let content = std::str::from_utf8(bytes).map_err(|_| PackageError::Invalid)?;
    let text = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or(PackageError::Invalid)?;
    let (frontmatter, suffix) = text.split_once("\n---").ok_or(PackageError::Invalid)?;
    if !suffix.is_empty() && !suffix.starts_with('\n') && !suffix.starts_with("\r\n") {
        return Err(PackageError::Invalid);
    }
    if frontmatter.len() > 32 * 1024 {
        return Err(PackageError::Invalid);
    }
    let fields: BTreeMap<String, Value> =
        serde_saphyr::from_str(frontmatter).map_err(|_| PackageError::Invalid)?;
    if fields.get("name").and_then(Value::as_str) != Some(expected)
        || !fields
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|value| valid_text(value, 1024, false, true))
    {
        return Err(PackageError::Invalid);
    }
    Ok(())
}

fn validate_mcp(
    bytes: &[u8],
    expected: &str,
    platform: &Platform,
    manifest: &PluginManifest,
    index: &EntryIndex,
) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| PackageError::Invalid)?;
    let object = value.as_object().ok_or(PackageError::Invalid)?;
    if object.len() != 1 {
        return Err(PackageError::Invalid);
    }
    let servers = object
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or(PackageError::Invalid)?;
    if servers.len() != 1 {
        return Err(PackageError::Invalid);
    }
    let server = servers
        .get(expected)
        .and_then(Value::as_object)
        .ok_or(PackageError::Invalid)?;
    let allowed = [
        "command",
        "args",
        "cwd",
        "env",
        "url",
        "headers",
        "transport",
        "type",
        "startupTimeoutMs",
        "toolTimeoutMs",
        "displayName",
        "description",
    ];
    if server.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(PackageError::Invalid);
    }
    if server
        .get("displayName")
        .is_some_and(|v| v.as_str().is_none_or(|v| !valid_text(v, 128, false, false)))
        || server
            .get("description")
            .is_some_and(|v| v.as_str().is_none_or(|v| !valid_text(v, 4096, true, true)))
    {
        return Err(PackageError::Invalid);
    }
    let command = server.get("command").and_then(Value::as_str);
    let url = server.get("url").and_then(Value::as_str);
    if command.is_some() == url.is_some() {
        return Err(PackageError::Invalid);
    }
    for field in ["transport", "type"] {
        if let Some(hint) = server.get(field) {
            let valid = if command.is_some() {
                ["local", "stdio"].as_slice()
            } else {
                ["remote", "http", "streamable-http", "streamable_http"].as_slice()
            };
            if hint.as_str().is_none_or(|value| !valid.contains(&value)) {
                return Err(PackageError::Invalid);
            }
        }
    }
    if let Some(command) = command {
        if !valid_text(command, 4096, false, false)
            || command.len() > 4096
            || server.contains_key("url")
            || server.contains_key("headers")
        {
            return Err(PackageError::Invalid);
        }
        if let Some(path) = command.strip_prefix("${plugin_root}/") {
            let entry = index.get(path).ok_or(PackageError::Invalid)?;
            if entry.kind != EntryKind::File
                || platform.os != "windows" && entry.mode & 0o111 == 0
                || path.starts_with("runtimes/")
                    && !path.starts_with(&format!("runtimes/{}/", platform.key()))
            {
                return Err(PackageError::Invalid);
            }
        } else if !(command.starts_with("${software.")
            && command.ends_with('}')
            && manifest
                .external_software
                .iter()
                .any(|s| command == format!("${{software.{}}}", s.name)))
        {
            return Err(PackageError::Invalid);
        }
    }
    if let Some(url) = url {
        let parsed = url::Url::parse(url).map_err(|_| PackageError::Invalid)?;
        if parsed.username() != ""
            || parsed.password().is_some()
            || parsed.fragment().is_some()
            || parsed.scheme() != "https"
                && !(parsed.scheme() == "http"
                    && parsed.host_str().is_some_and(|host| {
                        host == "localhost"
                            || host == "::1"
                            || host
                                .parse::<std::net::Ipv4Addr>()
                                .is_ok_and(|ip| ip.octets()[0] == 127)
                    }))
            || server.contains_key("args")
            || server.contains_key("cwd")
            || server.contains_key("env")
        {
            return Err(PackageError::Invalid);
        }
    }
    for (field, max) in [
        ("startupTimeoutMs", 60_000_u64),
        ("toolTimeoutMs", 3_600_000),
    ] {
        if server
            .get(field)
            .is_some_and(|v| v.as_u64().is_none_or(|v| v == 0 || v > max))
        {
            return Err(PackageError::Invalid);
        }
    }
    for (field, usage) in [("command", "command"), ("url", "url"), ("cwd", "cwd")] {
        if let Some(value) = server.get(field).and_then(Value::as_str) {
            validate_references(value, usage, manifest)?;
        }
    }
    if let Some(args) = server.get("args") {
        let args = args.as_array().ok_or(PackageError::Invalid)?;
        if args.len() > 64 {
            return Err(PackageError::Invalid);
        }
        for value in args {
            let value = value.as_str().ok_or(PackageError::Invalid)?;
            if !valid_text(value, 4096, true, false) {
                return Err(PackageError::Invalid);
            }
            validate_references(value, "args", manifest)?;
            if let Some(path) = value.strip_prefix("${plugin_root}/")
                && index
                    .get(path)
                    .is_none_or(|entry| entry.kind != EntryKind::File)
            {
                return Err(PackageError::Invalid);
            }
        }
    }
    if let Some(cwd) = server.get("cwd").and_then(Value::as_str) {
        let path = cwd
            .strip_prefix("${plugin_root}/")
            .ok_or(PackageError::Invalid)?;
        if !index
            .keys()
            .any(|entry| entry.starts_with(&format!("{path}/")))
        {
            return Err(PackageError::Invalid);
        }
    }
    for (field, usage) in [("env", "env"), ("headers", "headers")] {
        if let Some(values) = server.get(field) {
            let values = values.as_object().ok_or(PackageError::Invalid)?;
            if values.len() > 64 {
                return Err(PackageError::Invalid);
            }
            for (key, value) in values {
                let valid = if field == "env" {
                    key.as_bytes()
                        .first()
                        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                } else {
                    !key.is_empty()
                        && key
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
                        && !matches!(
                            key.to_ascii_lowercase().as_str(),
                            "host" | "content-length" | "accept" | "content-type"
                        )
                        && !key.to_ascii_lowercase().starts_with("mcp-")
                };
                if !valid {
                    return Err(PackageError::Invalid);
                }
                let value = value.as_str().ok_or(PackageError::Invalid)?;
                if !valid_text(value, 4096, true, false) {
                    return Err(PackageError::Invalid);
                }
                validate_references(value, usage, manifest)?;
            }
        }
    }
    Ok(())
}

fn validate_references(value: &str, usage: &str, manifest: &PluginManifest) -> Result<()> {
    let mut remaining = value;
    while let Some(start) = remaining.find("${") {
        let tail = &remaining[start + 2..];
        let end = tail.find('}').ok_or(PackageError::Invalid)?;
        let key = &tail[..end];
        let allowed = key == "plugin_root" && matches!(usage, "command" | "args" | "cwd" | "env")
            || key.strip_prefix("config.").is_some_and(|name| {
                matches!(usage, "env" | "headers")
                    && manifest.configuration.iter().any(|c| c.name == name)
            })
            || key.strip_prefix("software.").is_some_and(|name| {
                matches!(usage, "command" | "args" | "env")
                    && manifest.external_software.iter().any(|s| s.name == name)
            });
        if !allowed {
            return Err(PackageError::Invalid);
        }
        remaining = &tail[end + 1..];
    }
    Ok(())
}
