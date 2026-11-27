//! 已配对智能终端声明的 MCP 服务校验与受管定义投影；URL 宿主严格绑定到真实对端 IP。

use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
};

use assistant_protocol::{DeviceId, McpServerSource};
use assistant_runtime::{ManagedMcpServerDefinition, device_mcp_server_key};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::protocol::{DeviceMcpScheme, DeviceMcpServerDeclaration};

const MAX_DECLARED_MCP_SERVERS: usize = 4;
const MAX_DISPLAY_NAME_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_PATH_BYTES: usize = 256;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_VALUE_BYTES: usize = 4096;

/// 将设备在认证 `hello` 中附带的 MCP 声明转换成 `ManagedMcpServerDefinition`。
///
/// 非法、越界或试图用 `http://` 指向非私网/非回环对端的条目会被安全跳过，不阻断设备正常上线。
pub(super) fn build_device_mcp_definitions(
    device_id: &DeviceId,
    device_display_name: &str,
    peer_ip: IpAddr,
    declarations: &[DeviceMcpServerDeclaration],
) -> Vec<ManagedMcpServerDefinition> {
    let canonical_ip = peer_ip.to_canonical();
    let mut definitions = Vec::new();
    let mut seen_keys = HashSet::new();

    for decl in declarations.iter().take(MAX_DECLARED_MCP_SERVERS) {
        if decl.port == 0 || !is_valid_mcp_path(&decl.path) {
            continue;
        }
        if decl.scheme == DeviceMcpScheme::Http && !is_private_or_link_local(canonical_ip) {
            continue;
        }
        let Some(server_key) = device_mcp_server_key(device_id, decl.id.as_deref()) else {
            continue;
        };
        if !seen_keys.insert(server_key.clone()) {
            continue;
        }
        let Some(headers) = sanitize_headers(&decl.headers) else {
            continue;
        };
        let display_name = sanitize_display_name(&decl.display_name, device_display_name);
        let description = sanitize_description(&decl.description, device_display_name);
        let scheme = match decl.scheme {
            DeviceMcpScheme::Http => "http",
            DeviceMcpScheme::Https => "https",
        };
        let url = match canonical_ip {
            IpAddr::V4(v4) => format!("{scheme}://{v4}:{}{}", decl.port, decl.path),
            IpAddr::V6(v6) => format!("{scheme}://[{v6}]:{}{}", decl.port, decl.path),
        };

        let headers_map = headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect::<Map<String, Value>>();
        let mut raw = Map::new();
        raw.insert("enabled".to_owned(), Value::Bool(true));
        raw.insert(
            "displayName".to_owned(),
            Value::String(display_name.clone()),
        );
        raw.insert("description".to_owned(), Value::String(description.clone()));
        raw.insert("url".to_owned(), Value::String(url.clone()));
        if !headers_map.is_empty() {
            raw.insert("headers".to_owned(), Value::Object(headers_map));
        }

        let definition_digest = compute_definition_digest(
            device_id,
            server_key.as_str(),
            &url,
            &display_name,
            &description,
            &headers,
        );

        definitions.push(ManagedMcpServerDefinition {
            source: McpServerSource::Device(device_id.to_string()),
            server_key,
            definition_digest,
            source_enabled: true,
            source_available: true,
            ready: true,
            raw,
            parameter_bindings: BTreeMap::new(),
            parameters: Vec::new(),
            plugin_settings_revision: None,
            plugin_sha256: None,
            configuration: None,
        });
    }

    definitions
}

pub(super) fn is_safe_http_subpath(path: &str, max_bytes: usize) -> bool {
    if !path.starts_with('/') || path.len() > max_bytes {
        return false;
    }
    if path
        .bytes()
        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || path.contains(['?', '#', '\\'])
    {
        return false;
    }
    !path
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
}

fn is_valid_mcp_path(path: &str) -> bool {
    is_safe_http_subpath(path, MAX_PATH_BYTES)
}

pub(super) fn is_private_or_link_local(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

fn sanitize_display_name(raw: &str, fallback_device_name: &str) -> String {
    let trimmed = raw.trim();
    let base = if trimmed.is_empty() {
        fallback_device_name.trim()
    } else {
        trimmed
    };
    truncate_utf8(base, MAX_DISPLAY_NAME_BYTES)
}

fn sanitize_description(raw: &str, fallback_device_name: &str) -> String {
    let trimmed = raw.trim();
    let base = if trimmed.is_empty() {
        format!("来自智能终端 {fallback_device_name} 的 MCP 工具服务")
    } else {
        trimmed.to_owned()
    };
    truncate_utf8(&base, MAX_DESCRIPTION_BYTES)
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_owned()
}

fn sanitize_headers(raw: &BTreeMap<String, String>) -> Option<Vec<(String, String)>> {
    if raw.len() > MAX_HEADERS {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len());
    for (name, value) in raw {
        if !is_valid_header_name(name)
            || value.len() > MAX_HEADER_VALUE_BYTES
            || value.contains(['\r', '\n', '\0'])
        {
            return None;
        }
        out.push((name.clone(), value.clone()));
    }
    Some(out)
}

fn is_valid_header_name(name: &str) -> bool {
    if name.is_empty()
        || !name.bytes().all(|byte| {
            matches!(
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
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "host" | "content-length" | "accept" | "content-type"
    ) && !lower.starts_with("mcp-")
}

fn compute_definition_digest(
    device_id: &DeviceId,
    server_key: &str,
    url: &str,
    display_name: &str,
    description: &str,
    headers: &[(String, String)],
) -> String {
    let mut hash = Sha256::new();
    for field in [
        device_id.as_str(),
        server_key,
        url,
        display_name,
        description,
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update((headers.len() as u64).to_be_bytes());
    for (k, v) in headers {
        hash.update((k.len() as u64).to_be_bytes());
        hash.update(k.as_bytes());
        hash.update((v.len() as u64).to_be_bytes());
        hash.update(v.as_bytes());
    }
    let digest = hash.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn decl(port: u16, path: &str, scheme: DeviceMcpScheme) -> DeviceMcpServerDeclaration {
        DeviceMcpServerDeclaration {
            id: Some("ESP_Pika".to_owned()),
            display_name: "ESP-Pika Gateway".to_owned(),
            description: "硬件与声纹 MCP".to_owned(),
            scheme,
            port,
            path: path.to_owned(),
            headers: BTreeMap::from([
                ("X-Agent-ID".to_owned(), "ez-assistant".to_owned()),
                ("X-Speaker-Role".to_owned(), "owner".to_owned()),
            ]),
        }
    }

    #[test]
    fn pins_url_to_canonical_peer_ip_and_keeps_server_key_stable_across_ip_changes() {
        let device_id = DeviceId::new("device-TestKey01").unwrap();
        let d1 = build_device_mcp_definitions(
            &device_id,
            "Gateway A",
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0114)), // ::ffff:192.168.1.20
            &[decl(8080, "/mcp", DeviceMcpScheme::Http)],
        );
        let d2 = build_device_mcp_definitions(
            &device_id,
            "Gateway Renamed",
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42)),
            &[decl(8088, "/mcp", DeviceMcpScheme::Http)],
        );
        assert_eq!(d1.len(), 1);
        assert_eq!(d2.len(), 1);
        assert_eq!(d1[0].server_key, d2[0].server_key);
        assert_eq!(d1[0].raw["url"], "http://192.168.1.20:8080/mcp");
        assert_eq!(d2[0].raw["url"], "http://10.0.0.42:8088/mcp");
        assert_ne!(d1[0].definition_digest, d2[0].definition_digest);

        let v6 = build_device_mcp_definitions(
            &device_id,
            "Gateway V6",
            IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            &[decl(8080, "/mcp", DeviceMcpScheme::Http)],
        );
        assert_eq!(v6[0].raw["url"], "http://[fd00::1]:8080/mcp");
    }

    #[test]
    fn rejects_public_http_bad_paths_zero_port_and_forbidden_headers() {
        let device_id = DeviceId::new("device-Validation01").unwrap();
        let public_ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let lan_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5));

        assert!(
            build_device_mcp_definitions(
                &device_id,
                "Dev",
                public_ip,
                &[decl(8080, "/mcp", DeviceMcpScheme::Http)],
            )
            .is_empty()
        );
        assert_eq!(
            build_device_mcp_definitions(
                &device_id,
                "Dev",
                public_ip,
                &[decl(8443, "/mcp", DeviceMcpScheme::Https)],
            )
            .len(),
            1
        );
        for bad_path in ["mcp", "/mcp?x=1", "/mcp#frag", "/../admin", "/mcp bad"] {
            assert!(
                build_device_mcp_definitions(
                    &device_id,
                    "Dev",
                    lan_ip,
                    &[decl(8080, bad_path, DeviceMcpScheme::Http)],
                )
                .is_empty(),
                "path {bad_path} should be rejected"
            );
        }
        assert!(
            build_device_mcp_definitions(
                &device_id,
                "Dev",
                lan_ip,
                &[decl(0, "/mcp", DeviceMcpScheme::Http)],
            )
            .is_empty()
        );
        let mut bad_header = decl(8080, "/mcp", DeviceMcpScheme::Http);
        bad_header
            .headers
            .insert("MCP-Session-Id".to_owned(), "evil".to_owned());
        assert!(build_device_mcp_definitions(&device_id, "Dev", lan_ip, &[bad_header]).is_empty());
    }
}
