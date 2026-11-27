//! 已配对智能终端声明的控制台元数据校验、快照投影与受控反代目标解析。

use std::net::IpAddr;

use assistant_protocol::DeviceConsoleSnapshot;
use thiserror::Error;

use super::{
    mcp::{is_private_or_link_local, is_safe_http_subpath},
    protocol::{DeviceConsoleDeclaration, DeviceMcpScheme},
};

const MAX_CONSOLE_PREFIX_BYTES: usize = 128;
const MAX_CONSOLE_PROXY_PATH_BYTES: usize = 1024;
const MAX_CONSOLE_QUERY_BYTES: usize = 1024;

/// Host 已验证并绑定到真实对端 IP 的终端控制台上游目标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeviceConsoleTarget {
    pub(crate) scheme: DeviceMcpScheme,
    pub(crate) host: IpAddr,
    pub(crate) port: u16,
    pub(crate) api_prefix: String,
    pub(crate) ui_path: Option<String>,
}

/// 控制台代理相对路径校验错误。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ConsoleProxyPathError {
    #[error("device console path is invalid")]
    InvalidPath,
    #[error("device console path is forbidden")]
    ForbiddenPath,
}

/// 根据设备认证连接时的真实对端 IP 与 `hello.console` 声明构造受控控制台目标。
///
/// 非法路径、越界端口或试图用 `http://` 指向非私网/非回环对端的声明会被安全忽略。
pub(super) fn build_device_console_target(
    peer_ip: Option<IpAddr>,
    declaration: Option<&DeviceConsoleDeclaration>,
) -> Option<DeviceConsoleTarget> {
    let decl = declaration?;
    let peer_ip = peer_ip?.to_canonical();
    if decl.port == 0 {
        return None;
    }
    if decl.scheme == DeviceMcpScheme::Http && !is_private_or_link_local(peer_ip) {
        return None;
    }

    let raw_prefix = decl.api_prefix.trim();
    if !is_safe_http_subpath(raw_prefix, MAX_CONSOLE_PREFIX_BYTES) {
        return None;
    }
    let normalized_prefix = normalize_prefix(raw_prefix);

    let ui_path = match decl.ui_path.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(path) => {
            if !is_safe_http_subpath(path, MAX_CONSOLE_PREFIX_BYTES) {
                return None;
            }
            Some(path.to_owned())
        }
    };

    Some(DeviceConsoleTarget {
        scheme: decl.scheme,
        host: peer_ip,
        port: decl.port,
        api_prefix: normalized_prefix,
        ui_path,
    })
}

/// 将已验证的控制台目标投影为跨边界快照。
pub(super) fn to_snapshot(target: &DeviceConsoleTarget) -> DeviceConsoleSnapshot {
    let scheme = scheme_str(target.scheme);
    let authority = format_authority(target.host, target.port);
    let direct_suffix = target.ui_path.as_deref().unwrap_or("/");
    DeviceConsoleSnapshot {
        api_prefix: target.api_prefix.clone(),
        ui_path: target.ui_path.clone(),
        direct_url: Some(format!("{scheme}://{authority}{direct_suffix}")),
    }
}

/// 将 Host 收到的控制台相对路径与查询参数解析为固定对端的上游 URL。
///
/// 仅允许访问声明的 `api_prefix` 或 `/media/` 下的合法子路径，并严格屏蔽 `settings/deepseek`。
pub(crate) fn resolve_proxy_url(
    target: &DeviceConsoleTarget,
    relative_path: &str,
    raw_query: Option<&str>,
) -> Result<String, ConsoleProxyPathError> {
    let trimmed = relative_path.trim_start_matches('/');
    if trimmed.is_empty()
        || trimmed.len() > MAX_CONSOLE_PROXY_PATH_BYTES
        || trimmed
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || trimmed.contains(['?', '#', '\\'])
    {
        return Err(ConsoleProxyPathError::InvalidPath);
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%00")
    {
        return Err(ConsoleProxyPathError::InvalidPath);
    }

    let segments: Vec<&str> = trimmed.split('/').collect();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return Err(ConsoleProxyPathError::InvalidPath);
    }

    let normalized_path = format!("/{trimmed}");
    if contains_forbidden_deepseek_route(&normalized_path, &target.api_prefix) {
        return Err(ConsoleProxyPathError::ForbiddenPath);
    }

    let allowed = is_within_prefix(&normalized_path, &target.api_prefix)
        || is_within_prefix(&normalized_path, "/media");
    if !allowed {
        return Err(ConsoleProxyPathError::ForbiddenPath);
    }

    let query_suffix = match raw_query {
        Some(q) if !q.is_empty() => {
            if q.len() > MAX_CONSOLE_QUERY_BYTES
                || q.bytes()
                    .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
                || q.contains('#')
            {
                return Err(ConsoleProxyPathError::InvalidPath);
            }
            format!("?{q}")
        }
        _ => String::new(),
    };

    let scheme = scheme_str(target.scheme);
    let authority = format_authority(target.host, target.port);
    Ok(format!(
        "{scheme}://{authority}{normalized_path}{query_suffix}"
    ))
}

fn normalize_prefix(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn is_within_prefix(normalized_path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return normalized_path.starts_with('/');
    }
    normalized_path == prefix
        || normalized_path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn contains_forbidden_deepseek_route(normalized_path: &str, api_prefix: &str) -> bool {
    let lower_path = normalized_path.to_ascii_lowercase();
    let check_subpath = |candidate: &str| {
        let s = candidate.trim_start_matches('/');
        s == "settings/deepseek" || s.starts_with("settings/deepseek/")
    };
    if check_subpath(&lower_path) {
        return true;
    }
    let lower_prefix = api_prefix.to_ascii_lowercase();
    if let Some(rest) = lower_path.strip_prefix(&lower_prefix)
        && check_subpath(rest)
    {
        return true;
    }
    false
}

fn scheme_str(scheme: DeviceMcpScheme) -> &'static str {
    match scheme {
        DeviceMcpScheme::Http => "http",
        DeviceMcpScheme::Https => "https",
    }
}

fn format_authority(host: IpAddr, port: u16) -> String {
    match host {
        IpAddr::V4(v4) => format!("{v4}:{port}"),
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn sample_decl() -> DeviceConsoleDeclaration {
        DeviceConsoleDeclaration {
            scheme: DeviceMcpScheme::Http,
            port: 8000,
            api_prefix: "/api/v1".to_owned(),
            ui_path: Some("/ui/".to_owned()),
        }
    }

    #[test]
    fn build_target_pins_peer_ip_and_rejects_public_http() {
        let public_ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(build_device_console_target(Some(public_ip), Some(&sample_decl())).is_none());

        let https_decl = DeviceConsoleDeclaration {
            scheme: DeviceMcpScheme::Https,
            ..sample_decl()
        };
        assert!(build_device_console_target(Some(public_ip), Some(&https_decl)).is_some());

        let lan_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        let target =
            build_device_console_target(Some(lan_ip), Some(&sample_decl())).expect("lan http ok");
        let snapshot = to_snapshot(&target);
        assert_eq!(
            snapshot.direct_url.as_deref(),
            Some("http://192.168.1.20:8000/ui/")
        );
        assert_eq!(snapshot.api_prefix, "/api/v1");
        assert_eq!(snapshot.ui_path.as_deref(), Some("/ui/"));

        let v6_loopback = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let v6_target =
            build_device_console_target(Some(v6_loopback), Some(&sample_decl())).expect("v6 ok");
        let v6_snapshot = to_snapshot(&v6_target);
        assert_eq!(
            v6_snapshot.direct_url.as_deref(),
            Some("http://[::1]:8000/ui/")
        );
    }

    #[test]
    fn resolve_proxy_url_blocks_traversal_deepseek_and_unlisted_prefixes() {
        let target = build_device_console_target(
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            Some(&sample_decl()),
        )
        .expect("loopback target");

        assert_eq!(
            resolve_proxy_url(&target, "api/v1/devices", Some("limit=20")).expect("devices api"),
            "http://127.0.0.1:8000/api/v1/devices?limit=20"
        );
        assert_eq!(
            resolve_proxy_url(&target, "media/dev-1/latest.jpg", None).expect("media snapshot"),
            "http://127.0.0.1:8000/media/dev-1/latest.jpg"
        );

        assert_eq!(
            resolve_proxy_url(&target, "api/v1/settings/deepseek", None),
            Err(ConsoleProxyPathError::ForbiddenPath)
        );
        assert_eq!(
            resolve_proxy_url(&target, "api/v1/settings/DeepSeek/extra", None),
            Err(ConsoleProxyPathError::ForbiddenPath)
        );
        assert_eq!(
            resolve_proxy_url(&target, "settings/deepseek", None),
            Err(ConsoleProxyPathError::ForbiddenPath)
        );
        assert_eq!(
            resolve_proxy_url(&target, "healthz", None),
            Err(ConsoleProxyPathError::ForbiddenPath)
        );
        assert_eq!(
            resolve_proxy_url(&target, "api/v1/../settings/deepseek", None),
            Err(ConsoleProxyPathError::InvalidPath)
        );
        assert_eq!(
            resolve_proxy_url(&target, "api/v1/%2e%2e/secret", None),
            Err(ConsoleProxyPathError::InvalidPath)
        );
    }
}
