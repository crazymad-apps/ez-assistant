//! 工具参数离开 Runtime 前的唯一安全投影边界。

use assistant_protocol::{McpToolIdentity, ToolInputProjection, ToolInputSnapshot};
use serde_json::{Map, Value};

const MAX_DEPTH: usize = 6;
const MAX_COLLECTION_ITEMS: usize = 50;
const MAX_STRING_CHARS: usize = 2_000;
const MAX_PROJECTION_BYTES: usize = 16 * 1024;

#[derive(Default)]
struct ProjectionFlags {
    redacted: bool,
    truncated: bool,
}

pub(crate) fn project_tool_input(
    name: &str,
    arguments: &Value,
    mcp_identity: Option<&McpToolIdentity>,
) -> ToolInputProjection {
    let mut flags = ProjectionFlags::default();
    let value = if let Some(identity) = mcp_identity {
        let arguments = arguments
            .get("arguments")
            .filter(|value| value.is_object())
            .unwrap_or(&Value::Null);
        let safe = sanitize_json(arguments, 0, &mut flags);
        ToolInputSnapshot::Mcp {
            identity: identity.clone(),
            arguments_json: serialize_json(&safe, &mut flags),
        }
    } else if name == "inspect_images" {
        ToolInputSnapshot::ImageInspection {
            image_paths: string_array(arguments.get("image_paths"), &mut flags),
            goal: string_field(arguments, "goal", &mut flags),
            background: arguments
                .get("background")
                .and_then(Value::as_str)
                .map(|value| bounded_string(value, &mut flags)),
        }
    } else if name == "delegate_task" {
        ToolInputSnapshot::Delegation {
            title: string_field(arguments, "title", &mut flags),
            task_summary: arguments
                .get("task")
                .or_else(|| arguments.get("task_summary"))
                .and_then(Value::as_str)
                .map_or_else(String::new, |value| bounded_string(value, &mut flags)),
        }
    } else if name.contains("shell") {
        ToolInputSnapshot::Shell {
            command: string_field(arguments, "command", &mut flags),
            working_directory: arguments
                .get("working_directory")
                .or_else(|| arguments.get("workdir"))
                .and_then(Value::as_str)
                .map_or_else(String::new, |value| bounded_string(value, &mut flags)),
            timeout_ms: arguments
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            process_mode: string_field(arguments, "process_mode", &mut flags),
        }
    } else if arguments.get("paths").and_then(Value::as_array).is_some() {
        ToolInputSnapshot::Files {
            operation: bounded_string(name, &mut flags),
            paths: string_array(arguments.get("paths"), &mut flags),
        }
    } else if let Some(path) = arguments.get("path").and_then(Value::as_str) {
        ToolInputSnapshot::File {
            operation: bounded_string(name, &mut flags),
            path: bounded_string(path, &mut flags),
        }
    } else {
        let safe = sanitize_json(arguments, 0, &mut flags);
        ToolInputSnapshot::General {
            summary: serialize_json(&safe, &mut flags),
        }
    };
    finish_projection(value, flags)
}

/// 返回可用于详情代码块的安全 JSON，以及本次处理是否隐去或截断过内容。
pub(crate) fn project_json(arguments: &Value) -> (String, bool, bool) {
    let mut flags = ProjectionFlags::default();
    let safe = sanitize_json(arguments, 0, &mut flags);
    let value = match serde_json::to_string_pretty(&safe) {
        Ok(encoded) if encoded.len() <= MAX_PROJECTION_BYTES => encoded,
        Ok(_) | Err(_) => {
            flags.truncated = true;
            "null".to_owned()
        }
    };
    (value, flags.redacted, flags.truncated)
}

fn finish_projection(value: ToolInputSnapshot, mut flags: ProjectionFlags) -> ToolInputProjection {
    let projection = ToolInputProjection {
        value,
        redacted: flags.redacted,
        truncated: flags.truncated,
    };
    let exceeds_limit = serde_json::to_vec(&projection)
        .map(|encoded| encoded.len() > MAX_PROJECTION_BYTES)
        .unwrap_or(true);
    if exceeds_limit {
        flags.truncated = true;
        ToolInputProjection {
            value: ToolInputSnapshot::Unavailable,
            redacted: flags.redacted,
            truncated: true,
        }
    } else {
        projection
    }
}

fn sanitize_json(value: &Value, depth: usize, flags: &mut ProjectionFlags) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(value) => Value::String(bounded_string(value, flags)),
        Value::Array(values) => {
            if depth >= MAX_DEPTH {
                flags.truncated = true;
                return Value::Null;
            }
            if values.len() > MAX_COLLECTION_ITEMS {
                flags.truncated = true;
            }
            Value::Array(
                values
                    .iter()
                    .take(MAX_COLLECTION_ITEMS)
                    .map(|value| sanitize_json(value, depth + 1, flags))
                    .collect(),
            )
        }
        Value::Object(values) => {
            if depth >= MAX_DEPTH {
                flags.truncated = true;
                return Value::Null;
            }
            if values.len() > MAX_COLLECTION_ITEMS {
                flags.truncated = true;
            }
            Value::Object(
                values
                    .iter()
                    .take(MAX_COLLECTION_ITEMS)
                    .map(|(key, value)| {
                        let value = if is_sensitive_key(key) {
                            flags.redacted = true;
                            Value::String("***".to_owned())
                        } else {
                            sanitize_json(value, depth + 1, flags)
                        };
                        (key.clone(), value)
                    })
                    .collect::<Map<_, _>>(),
            )
        }
    }
}

fn serialize_json(value: &Value, flags: &mut ProjectionFlags) -> String {
    match serde_json::to_string(value) {
        Ok(encoded) if encoded.len() <= MAX_PROJECTION_BYTES => encoded,
        Ok(_) => {
            flags.truncated = true;
            "null".to_owned()
        }
        Err(_) => {
            flags.truncated = true;
            "null".to_owned()
        }
    }
}

fn string_field(arguments: &Value, key: &str, flags: &mut ProjectionFlags) -> String {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map_or_else(String::new, |value| bounded_string(value, flags))
}

fn string_array(value: Option<&Value>, flags: &mut ProjectionFlags) -> Vec<String> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    if values.len() > MAX_COLLECTION_ITEMS {
        flags.truncated = true;
    }
    values
        .iter()
        .take(MAX_COLLECTION_ITEMS)
        .filter_map(Value::as_str)
        .map(|value| bounded_string(value, flags))
        .collect()
}

fn bounded_string(value: &str, flags: &mut ProjectionFlags) -> String {
    let mut chars = value.chars();
    let result = chars.by_ref().take(MAX_STRING_CHARS).collect::<String>();
    if chars.next().is_some() {
        flags.truncated = true;
    }
    result
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| !matches!(character, '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "authorization"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "token"
            | "password"
            | "passwd"
            | "secret"
            | "clientsecret"
            | "privatekey"
            | "cookie"
            | "setcookie"
    )
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{McpServerKey, McpToolIdentity, ToolInputSnapshot};
    use serde_json::json;

    use super::{MAX_COLLECTION_ITEMS, project_json, project_tool_input};

    #[test]
    fn general_projection_redacts_normalized_sensitive_keys_recursively() {
        let projection = project_tool_input(
            "custom_tool",
            &json!({
                "API-Key": "secret-a",
                "nested": {"set_cookie": "secret-b", "visible": "ok"}
            }),
            None,
        );
        assert!(projection.redacted);
        assert!(!projection.truncated);
        let ToolInputSnapshot::General { summary } = projection.value else {
            panic!("expected general projection");
        };
        assert!(summary.contains("***"));
        assert!(!summary.contains("secret-a"));
        assert!(!summary.contains("secret-b"));
    }

    #[test]
    fn projection_marks_depth_collection_and_string_limits() {
        let values = (0..=MAX_COLLECTION_ITEMS).collect::<Vec<_>>();
        let projection = project_tool_input(
            "custom_tool",
            &json!({"values": values, "long": "x".repeat(2_001)}),
            None,
        );
        assert!(projection.truncated);
        assert!(
            serde_json::to_vec(&projection)
                .expect("serialize projection")
                .len()
                <= 16 * 1024
        );
    }

    #[test]
    fn mcp_projection_only_exposes_safe_arguments() {
        let identity = McpToolIdentity {
            server_key: McpServerKey::new("server").expect("server key"),
            server_display_name: "Server".to_owned(),
            tool_name: "run".to_owned(),
        };
        let projection = project_tool_input(
            "call_mcp_tool",
            &json!({"server": "server", "tool": "run", "arguments": {"access_token": "raw", "query": "safe"}}),
            Some(&identity),
        );
        assert!(projection.redacted);
        let ToolInputSnapshot::Mcp { arguments_json, .. } = projection.value else {
            panic!("expected mcp projection");
        };
        assert!(!arguments_json.contains("raw"));
        assert!(arguments_json.contains("safe"));
    }

    #[test]
    fn detail_json_uses_the_same_redaction_boundary() {
        let (value, redacted, truncated) = project_json(&json!({"Authorization": "raw"}));
        assert!(redacted);
        assert!(!truncated);
        assert_eq!(value, "{\n  \"Authorization\": \"***\"\n}");
    }
}
