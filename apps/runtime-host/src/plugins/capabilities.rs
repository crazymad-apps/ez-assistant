//! 已生效插件包的 MCP 描述符读取与参数展开；只消费 Runtime 提供的包引用。

use std::{collections::BTreeMap, fs, path::Path};

use assistant_protocol::{McpDiagnosticCode, McpDiagnosticSnapshot, McpServerKey};
use assistant_runtime::{
    ManagedMcpServerDefinition, McpServerConfig, PluginMcpDiscovery, StoredPlugin,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::PluginManifest;

pub(super) fn discover_mcp(user_root: &Path, plugins: Vec<StoredPlugin>) -> PluginMcpDiscovery {
    let mut result = PluginMcpDiscovery::default();
    for plugin in plugins {
        discover_one(user_root, &plugin, &mut result);
    }
    result
}

fn discover_one(user_root: &Path, plugin: &StoredPlugin, result: &mut PluginMcpDiscovery) {
    let Some(sha) = plugin.active_sha256.as_deref() else {
        return;
    };
    if !super::plugin_name(&plugin.name)
        || plugin.name.len() > 64
        || sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        result
            .diagnostics
            .push(diagnostic(None, "plugin package reference is invalid"));
        return;
    }
    let package = user_root
        .join("plugins")
        .join(&plugin.name)
        .join("packages")
        .join(sha);
    let manifest_bytes = match read_regular(&package.join("plugin.json")) {
        Some(bytes) => bytes,
        None => {
            result
                .diagnostics
                .push(diagnostic(None, "plugin package is unavailable"));
            return;
        }
    };
    if plugin.active_manifest_json.as_deref() != std::str::from_utf8(&manifest_bytes).ok() {
        result
            .diagnostics
            .push(diagnostic(None, "plugin package manifest changed"));
        return;
    }
    let Ok(manifest) = PluginManifest::parse(&manifest_bytes) else {
        result
            .diagnostics
            .push(diagnostic(None, "plugin package manifest is invalid"));
        return;
    };
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("macos", "x86_64") => "macos-x64",
        ("windows", "aarch64") => "windows-arm64",
        ("windows", "x86_64") => "windows-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        _ => {
            result
                .diagnostics
                .push(diagnostic(None, "current platform is unsupported"));
            return;
        }
    };
    if !manifest
        .platforms
        .iter()
        .any(|p| format!("{}-{}", p.os, p.arch) == platform)
    {
        result.diagnostics.push(diagnostic(
            None,
            "plugin does not support the current platform",
        ));
        return;
    }
    let parameters = parse_values(&plugin.parameters_json);
    let software = parse_values(&plugin.software_paths_json);
    let Some(parameters) = parameters else {
        result
            .diagnostics
            .push(diagnostic(None, "plugin parameters are invalid"));
        return;
    };
    let Some(software) = software else {
        result
            .diagnostics
            .push(diagnostic(None, "plugin software paths are invalid"));
        return;
    };
    for name in &manifest.mcp {
        let Ok(key) = McpServerKey::new(name.clone()) else {
            result
                .diagnostics
                .push(diagnostic(None, "plugin MCP server name is invalid"));
            continue;
        };
        result.definitions.push(ManagedMcpServerDefinition {
            source: assistant_protocol::McpServerSource::Plugin(plugin.name.clone()),
            server_key: key.clone(),
            definition_digest: sha.to_owned(),
            source_enabled: plugin.enabled,
            source_available: false,
            ready: false,
            raw: Map::new(),
            parameter_bindings: BTreeMap::new(),
            parameters: Vec::new(),
            plugin_settings_revision: None,
            plugin_sha256: Some(sha.to_owned()),
            configuration: None,
        });
        let definition = result
            .definitions
            .last_mut()
            .expect("definition just inserted");
        let scoped = package
            .join("mcp")
            .join(name)
            .join(format!("server.{platform}.json"));
        let common = package.join("mcp").join(name).join("server.json");
        let descriptor = if fs::symlink_metadata(&scoped).is_ok() {
            scoped
        } else {
            common
        };
        let Some(bytes) = read_regular(&descriptor) else {
            result.diagnostics.push(diagnostic(
                Some(key),
                "plugin MCP descriptor is unavailable",
            ));
            continue;
        };
        if bytes.len() > 256 * 1024 {
            result
                .diagnostics
                .push(diagnostic(Some(key), "plugin MCP descriptor is too large"));
            continue;
        }
        let raw = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value.get("mcpServers")?.get(name)?.as_object().cloned());
        let Some(mut raw) = raw else {
            result
                .diagnostics
                .push(diagnostic(Some(key), "plugin MCP descriptor is invalid"));
            continue;
        };
        definition.definition_digest = format!(
            "sha256:{:x}",
            Sha256::digest([sha.as_bytes(), &bytes].concat())
        );
        definition.source_available = true;
        for field in ["env", "headers"] {
            if let Some(values) = raw.get(field).and_then(Value::as_object) {
                for (key, value) in values {
                    let bindings = value.as_str().map(parameter_references).unwrap_or_default();
                    if !bindings.is_empty() {
                        definition.parameter_bindings.insert(key.clone(), bindings);
                    }
                }
            }
        }
        let original = raw.clone();
        if !expand_server(&mut raw, &package, &parameters, &software) {
            let mut missing = parameters.clone();
            for field in &manifest.configuration {
                missing.entry(field.name.clone()).or_default();
            }
            let mut empty_software = software.clone();
            for field in &manifest.external_software {
                empty_software.entry(field.name.clone()).or_default();
            }
            let mut partial = original;
            // 仅供脱敏管理投影，ready=false 时绝不将占位展开用于执行。
            let _ = expand_server(&mut partial, &package, &missing, &empty_software);
            definition.raw = partial;
            result.diagnostics.push(diagnostic(
                Some(key),
                "plugin MCP configuration is incomplete or conflicts with reserved environment",
            ));
            continue;
        }
        definition.raw = raw.clone();
        match McpServerConfig::from_plugin_descriptor(
            plugin.name.clone(),
            sha.to_owned(),
            key.clone(),
            raw,
        ) {
            Ok(_) => {
                definition.ready = true;
            }
            Err(_) => result
                .diagnostics
                .push(diagnostic(Some(key), "plugin MCP configuration is invalid")),
        }
    }
}

fn parameter_references(value: &str) -> Vec<String> {
    let mut remaining = value;
    let mut names = std::collections::BTreeSet::new();
    while let Some((_, after)) = remaining.split_once("${config.") {
        let Some((name, rest)) = after.split_once('}') else {
            break;
        };
        names.insert(name.to_owned());
        remaining = rest;
    }
    names.into_iter().collect()
}

fn parse_values(source: &str) -> Option<BTreeMap<String, String>> {
    serde_json::from_str(source).ok()
}

fn read_regular(path: &Path) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 * 1024 {
        return None;
    }
    fs::read(path).ok()
}

fn expand_server(
    raw: &mut Map<String, Value>,
    root: &Path,
    parameters: &BTreeMap<String, String>,
    software: &BTreeMap<String, String>,
) -> bool {
    for field in ["command", "url", "cwd"] {
        if let Some(value) = raw.get_mut(field) {
            let Some(input) = value.as_str() else {
                return false;
            };
            let Some(expanded) = expand(input, root, parameters, software) else {
                return false;
            };
            *value = Value::String(expanded);
        }
    }
    if let Some(args) = raw.get_mut("args") {
        let Some(args) = args.as_array_mut() else {
            return false;
        };
        for value in args {
            let Some(input) = value.as_str() else {
                return false;
            };
            let Some(expanded) = expand(input, root, parameters, software) else {
                return false;
            };
            *value = Value::String(expanded);
        }
    }
    for field in ["env", "headers"] {
        if let Some(values) = raw.get_mut(field) {
            let Some(values) = values.as_object_mut() else {
                return false;
            };
            if field == "env"
                && values.keys().any(|key| {
                    matches!(
                        key.as_str(),
                        "EZ_ASSISTANT_PLUGIN_ROOT"
                            | "EZ_ASSISTANT_PLUGIN_CONFIG_DIR"
                            | "EZ_ASSISTANT_PLUGIN_DATA_DIR"
                    )
                })
            {
                return false;
            }
            for value in values.values_mut() {
                let Some(input) = value.as_str() else {
                    return false;
                };
                let Some(expanded) = expand(input, root, parameters, software) else {
                    return false;
                };
                *value = Value::String(expanded);
            }
        }
    }
    if raw.contains_key("command") {
        let env = raw
            .entry("env")
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(env) = env.as_object_mut() else {
            return false;
        };
        let Some(plugin_dir) = root.parent().and_then(Path::parent) else {
            return false;
        };
        let values = [
            ("EZ_ASSISTANT_PLUGIN_ROOT", root.to_path_buf()),
            ("EZ_ASSISTANT_PLUGIN_CONFIG_DIR", plugin_dir.join("config")),
            ("EZ_ASSISTANT_PLUGIN_DATA_DIR", plugin_dir.join("data")),
        ];
        for (key, path) in values {
            let Some(path) = path.to_str() else {
                return false;
            };
            env.insert(key.to_owned(), Value::String(path.to_owned()));
        }
    }
    true
}

fn expand(
    input: &str,
    root: &Path,
    parameters: &BTreeMap<String, String>,
    software: &BTreeMap<String, String>,
) -> Option<String> {
    let root_text = root.to_str()?;
    let mut remaining = input;
    let mut output = String::new();
    while let Some(start) = remaining.find("${") {
        output.push_str(&remaining[..start]);
        let tail = &remaining[start + 2..];
        let end = tail.find('}')?;
        let key = &tail[..end];
        let value = if key == "plugin_root" {
            root_text
        } else if let Some(name) = key.strip_prefix("config.") {
            parameters.get(name)?.as_str()
        } else {
            let name = key.strip_prefix("software.")?;
            software.get(name)?.as_str()
        };
        output.push_str(value);
        remaining = &tail[end + 1..];
        if output.len() > 32 * 1024 {
            return None;
        }
    }
    output.push_str(remaining);
    if output.len() > 32 * 1024 {
        return None;
    }
    // 包内路径以 / 分隔，Windows canonical 根可能是 \\?\C:\...；字符串拼接会
    // 留下 verbatim 路径不接受的 /。通过原生 join 组合路径，保留长路径前缀及真实根。
    if input.starts_with("${plugin_root}/") {
        let relative = output.strip_prefix(root_text)?.strip_prefix('/')?;
        return root.join(relative).into_os_string().into_string().ok();
    }
    Some(output)
}

fn diagnostic(server_key: Option<McpServerKey>, message: &str) -> McpDiagnosticSnapshot {
    McpDiagnosticSnapshot {
        server_key,
        code: McpDiagnosticCode::InvalidConfig,
        field_path: None,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_runtime::{McpConnectionFactory, McpConnectionOptions, PluginSource};

    fn ready_server(discovery: &PluginMcpDiscovery) -> McpServerConfig {
        let definition = discovery
            .definitions
            .iter()
            .find(|definition| definition.ready)
            .expect("ready definition");
        let assistant_protocol::McpServerSource::Plugin(name) = &definition.source else {
            panic!("plugin fixture")
        };
        McpServerConfig::from_plugin_descriptor(
            name.clone(),
            definition.plugin_sha256.clone().unwrap(),
            definition.server_key.clone(),
            definition.raw.clone(),
        )
        .unwrap()
    }

    #[test]
    fn bundled_mcp_paths_resolve_under_canonical_plugin_root() {
        let home = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(home.path())
            .unwrap()
            .join("插件 中文 空格");
        let executables = [
            "runtimes/windows-x64/gorag-mcp.exe",
            "runtimes/windows-x64/python/python.exe",
        ];
        for relative in executables {
            let executable = root.join(relative);
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(&executable, b"fixture").unwrap();
            let mut raw = serde_json::json!({
                "command": format!("${{plugin_root}}/{relative}"),
                "args": ["${plugin_root}/mcp/entry.py"],
                "cwd": "${plugin_root}/mcp",
            })
            .as_object()
            .unwrap()
            .clone();
            assert!(expand_server(
                &mut raw,
                &root,
                &BTreeMap::new(),
                &BTreeMap::new()
            ));
            let command = Path::new(raw["command"].as_str().unwrap());
            assert_eq!(command, executable);
            assert!(command.is_file());
            assert_eq!(
                Path::new(raw["args"][0].as_str().unwrap()),
                root.join("mcp/entry.py")
            );
            assert_eq!(Path::new(raw["cwd"].as_str().unwrap()), root.join("mcp"));
        }
    }

    #[tokio::test]
    async fn managed_plugin_remote_descriptor_connects_to_controlled_server() {
        use axum::{
            Router,
            body::{Body, Bytes},
            http::{HeaderMap, Response, StatusCode},
            routing::post,
        };
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        async fn endpoint(headers: HeaderMap, body: Bytes) -> Response<Body> {
            assert_eq!(
                headers
                    .get("x-fixture")
                    .and_then(|value| value.to_str().ok()),
                Some("present")
            );
            let request: Value = serde_json::from_slice(&body).unwrap();
            let result = match request["method"].as_str() {
                Some("server/discover") => serde_json::json!({
                    "resultType":"complete", "supportedVersions":["2026-07-28"],
                    "capabilities":{"tools":{}}, "ttlMs":0, "cacheScope":"private"
                }),
                Some("tools/list") => serde_json::json!({
                    "resultType":"complete", "ttlMs":0, "cacheScope":"private", "tools":[]
                }),
                other => panic!("unexpected MCP method: {other:?}"),
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "jsonrpc":"2.0", "id":request["id"], "result":result
                    })
                    .to_string(),
                ))
                .unwrap()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/mcp", post(endpoint)))
                .await
                .unwrap();
        });
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".runtime-test");
        fs::create_dir_all(&base).unwrap();
        let home = tempfile::Builder::new()
            .prefix("c03-m2-remote-")
            .tempdir_in(base)
            .unwrap();
        let sha = "c".repeat(64);
        let root = home
            .path()
            .join("plugins/example-tools/packages")
            .join(&sha);
        fs::create_dir_all(root.join("mcp/example")).unwrap();
        let (os, arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => ("macos", "arm64"),
            ("macos", "x86_64") => ("macos", "x64"),
            ("windows", "aarch64") => ("windows", "arm64"),
            ("windows", "x86_64") => ("windows", "x64"),
            ("linux", "aarch64") => ("linux", "arm64"),
            ("linux", "x86_64") => ("linux", "x64"),
            _ => return,
        };
        let manifest = serde_json::json!({
            "format_version":1,"name":"example-tools","display_name":"Example",
            "description":"Remote MCP","version":"1.0.0",
            "platforms":[{"os":os,"arch":arch}],"min_host_version":"0.28.0",
            "skills":[],"mcp":["example"],
            "configuration":[{"name":"token","label":"Token","required":true,"secret":true}],
            "external_software":[]
        })
        .to_string();
        fs::write(root.join("plugin.json"), &manifest).unwrap();
        fs::write(
            root.join("mcp/example/server.json"),
            serde_json::json!({
                "mcpServers":{"example":{
                    "url":format!("http://{address}/mcp"),
                    "headers":{"x-fixture":"${config.token}"}
                }}
            })
            .to_string(),
        )
        .unwrap();
        let plugin = StoredPlugin {
            name: "example-tools".into(),
            source: PluginSource::Upload,
            active_version: Some("1.0.0".into()),
            active_sha256: Some(sha),
            active_manifest_json: Some(manifest),
            enabled: true,
            parameters_json: r#"{"token":"present"}"#.into(),
            software_paths_json: "{}".into(),
            market_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let discovery = discover_mcp(home.path(), vec![plugin]);
        assert!(discovery.diagnostics.is_empty());
        let factory = crate::mcp::HostMcpConnectionFactory::new(home.path().join("runtime"));
        let token = CancellationToken::new();
        let connection = tokio::time::timeout(
            Duration::from_secs(10),
            factory.connect(
                ready_server(&discovery),
                McpConnectionOptions {
                    max_concurrent_requests: 1,
                    control_timeout: Duration::from_secs(5),
                },
                token.clone(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            connection
                .list_tools_page(None, token.clone())
                .await
                .unwrap()
                .tools
                .is_empty()
        );
        connection.close(token).await.unwrap();
        service.abort();
        let _ = service.await;
    }

    #[test]
    fn plugin_descriptor_uses_managed_roots_and_required_parameters_without_leaking_values() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".runtime-test");
        fs::create_dir_all(&base).unwrap();
        let home = tempfile::Builder::new()
            .prefix("c03-m2-mcp-")
            .tempdir_in(base)
            .unwrap();
        let sha = "a".repeat(64);
        let root = home
            .path()
            .join("plugins/example-tools/packages")
            .join(&sha);
        fs::create_dir_all(root.join("mcp/example")).unwrap();
        let (os, arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => ("macos", "arm64"),
            ("macos", "x86_64") => ("macos", "x64"),
            ("windows", "aarch64") => ("windows", "arm64"),
            ("windows", "x86_64") => ("windows", "x64"),
            ("linux", "aarch64") => ("linux", "arm64"),
            ("linux", "x86_64") => ("linux", "x64"),
            _ => return,
        };
        let manifest = serde_json::json!({
            "format_version":1,"name":"example-tools","display_name":"Example",
            "description":"Example MCP","version":"1.0.0",
            "platforms":[{"os":os,"arch":arch}],"min_host_version":"0.28.0",
            "skills":[],"mcp":["example"],
            "configuration":[{"name":"token","label":"Token","required":true,"secret":true}],
            "external_software":[]
        })
        .to_string();
        fs::write(root.join("plugin.json"), &manifest).unwrap();
        fs::write(root.join("mcp/example/server.json"), serde_json::json!({
            "mcpServers":{"example":{"command":"${plugin_root}/bin/server","env":{"API_TOKEN":"${config.token}"}}}
        }).to_string()).unwrap();
        let mut plugin = StoredPlugin {
            name: "example-tools".into(),
            source: PluginSource::Upload,
            active_version: Some("1.0.0".into()),
            active_sha256: Some(sha),
            active_manifest_json: Some(manifest),
            enabled: true,
            parameters_json: r#"{"token":"secret-value"}"#.into(),
            software_paths_json: "{}".into(),
            market_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let ready = discover_mcp(home.path(), vec![plugin.clone()]);
        assert_eq!(ready.definitions.len(), 1);
        assert!(ready.diagnostics.is_empty());
        assert_eq!(ready_server(&ready).plugin_name(), Some("example-tools"));
        plugin.parameters_json = "{}".into();
        let missing = discover_mcp(home.path(), vec![plugin]);
        assert_eq!(missing.definitions.len(), 1);
        assert!(!missing.definitions[0].ready);
        assert_eq!(missing.diagnostics.len(), 1);
        assert!(!missing.diagnostics[0].message.contains("secret-value"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_plugin_stdio_descriptor_starts_and_calls_real_server() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".runtime-test");
        fs::create_dir_all(&base).unwrap();
        let home = tempfile::Builder::new()
            .prefix("c03-m2-stdio-")
            .tempdir_in(base)
            .unwrap();
        let sha = "b".repeat(64);
        let root = home
            .path()
            .join("plugins/example-tools/packages")
            .join(&sha);
        let (os, arch) = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => ("macos", "arm64"),
            ("macos", "x86_64") => ("macos", "x64"),
            ("linux", "aarch64") => ("linux", "arm64"),
            ("linux", "x86_64") => ("linux", "x64"),
            _ => return,
        };
        fs::create_dir_all(root.join("mcp/example")).unwrap();
        let runtime = root.join(format!("runtimes/{os}-{arch}"));
        fs::create_dir_all(&runtime).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_server.py"),
            runtime.join("server.py"),
        )
        .unwrap();
        let launcher = runtime.join("server");
        fs::write(
            &launcher,
            "#!/bin/sh\nexec python3 \"$(dirname \"$0\")/server.py\"\n",
        )
        .unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
        let manifest = serde_json::json!({
            "format_version":1,"name":"example-tools","display_name":"Example",
            "description":"Example MCP","version":"1.0.0",
            "platforms":[{"os":os,"arch":arch}],"min_host_version":"0.28.0",
            "skills":[],"mcp":["example"],"configuration":[],"external_software":[]
        })
        .to_string();
        fs::write(root.join("plugin.json"), &manifest).unwrap();
        fs::write(root.join("mcp/example/server.json"), serde_json::json!({
            "mcpServers":{"example":{"command":format!("${{plugin_root}}/runtimes/{os}-{arch}/server")}}
        }).to_string()).unwrap();
        let plugin = StoredPlugin {
            name: "example-tools".into(),
            source: PluginSource::Upload,
            active_version: Some("1.0.0".into()),
            active_sha256: Some(sha),
            active_manifest_json: Some(manifest),
            enabled: true,
            parameters_json: "{}".into(),
            software_paths_json: "{}".into(),
            market_status: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let discovery = discover_mcp(home.path(), vec![plugin]);
        assert!(
            discovery.diagnostics.is_empty(),
            "{:?}",
            discovery.diagnostics
        );
        let server = ready_server(&discovery);
        let factory = crate::mcp::HostMcpConnectionFactory::new(home.path().join("runtime"));
        let token = CancellationToken::new();
        let connection = tokio::time::timeout(
            Duration::from_secs(10),
            factory.connect(
                server,
                McpConnectionOptions {
                    max_concurrent_requests: 1,
                    control_timeout: Duration::from_secs(5),
                },
                token.clone(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let tools = connection
            .list_tools_page(None, token.clone())
            .await
            .unwrap();
        assert_eq!(tools.tools[0].name, "first_tool");
        let result = connection
            .call_tool_once("first_tool".into(), Map::new(), token.clone())
            .await
            .unwrap();
        assert!(!result.is_error);
        connection.close(token).await.unwrap();
        factory.shutdown().await.unwrap();
    }
}
