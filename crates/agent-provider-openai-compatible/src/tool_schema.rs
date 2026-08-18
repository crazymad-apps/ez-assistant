//! Provider function parameters 的 Schema 方言适配。

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

use crate::ToolSchemaDialect;

/// 按 Profile 声明的方言编码工具输入 Schema。
pub(crate) fn encode_tool_schema(
    schema: &Value,
    dialect: ToolSchemaDialect,
) -> Result<Value, String> {
    match dialect {
        ToolSchemaDialect::JsonSchema2020_12 => Ok(schema.clone()),
        ToolSchemaDialect::OpenAiFunctionSubset => {
            lower_schema(schema, schema, &mut HashSet::new())
        }
    }
}

/// 把 schemars 的 Draft 2020-12 输出降级到 function calling 的稳定公共子集。
///
/// 这里仅改变线上表达，不改变规范 ToolDefinition。运行时仍会使用原始 Rust 输入类型
/// 反序列化和校验参数，因此被子集省略的默认值、format 与条件必填约束不会失去保护。
fn lower_schema(
    value: &Value,
    root: &Value,
    resolving_refs: &mut HashSet<String>,
) -> Result<Value, String> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| lower_schema(value, root, resolving_refs))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object) => lower_object(object, root, resolving_refs),
        _ => Ok(value.clone()),
    }
}

fn lower_object(
    object: &Map<String, Value>,
    root: &Value,
    resolving_refs: &mut HashSet<String>,
) -> Result<Value, String> {
    let mut base = if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        resolve_reference(reference, root, resolving_refs)?
    } else {
        Value::Object(Map::new())
    };

    let mut lowered = Map::new();
    for (key, value) in object {
        match key.as_str() {
            "$ref" | "$schema" | "$defs" | "definitions" | "title" | "default" | "examples"
            | "deprecated" | "readOnly" | "writeOnly" | "minLength" | "maxLength" | "minItems"
            | "maxItems" => {}
            "format" if !supported_string_format(value) => {}
            "const" => {
                lowered.insert("enum".to_owned(), Value::Array(vec![value.clone()]));
            }
            "type" => lower_type(value, &mut lowered),
            "oneOf" => {
                let alternatives = lower_alternatives(value, root, resolving_refs)?;
                merge_alternatives(&mut lowered, alternatives)?;
            }
            _ => {
                lowered.insert(key.clone(), lower_schema(value, root, resolving_refs)?);
            }
        }
    }

    merge_schema(&mut base, Value::Object(lowered))?;
    Ok(base)
}

fn resolve_reference(
    reference: &str,
    root: &Value,
    resolving_refs: &mut HashSet<String>,
) -> Result<Value, String> {
    let Some(pointer) = reference.strip_prefix('#') else {
        return Err(format!(
            "external schema reference is not supported: {reference}"
        ));
    };
    if !resolving_refs.insert(reference.to_owned()) {
        return Err(format!(
            "recursive schema reference is not supported: {reference}"
        ));
    }
    let resolved = root
        .pointer(pointer)
        .ok_or_else(|| format!("schema reference does not exist: {reference}"))?;
    let result = lower_schema(resolved, root, resolving_refs);
    resolving_refs.remove(reference);
    result
}

fn lower_type(value: &Value, output: &mut Map<String, Value>) {
    let Value::Array(types) = value else {
        if value != "null" {
            output.insert("type".to_owned(), value.clone());
        }
        return;
    };
    // function calling 的公共子集没有 null 类型。Option 字段本身不在 required 中，
    // 因此省略 null 分支不会改变模型可表达的有效调用。
    let non_null = types
        .iter()
        .filter(|kind| *kind != "null")
        .cloned()
        .collect::<Vec<_>>();
    match non_null.as_slice() {
        [] => {}
        [only] => {
            output.insert("type".to_owned(), only.clone());
        }
        many => {
            output.insert(
                "anyOf".to_owned(),
                Value::Array(
                    many.iter()
                        .map(|kind| {
                            Value::Object(Map::from_iter([("type".to_owned(), kind.clone())]))
                        })
                        .collect(),
                ),
            );
        }
    }
}

fn lower_alternatives(
    value: &Value,
    root: &Value,
    resolving_refs: &mut HashSet<String>,
) -> Result<Vec<Value>, String> {
    let Value::Array(alternatives) = value else {
        return Err("oneOf must be an array".to_owned());
    };
    alternatives
        .iter()
        .map(|alternative| lower_schema(alternative, root, resolving_refs))
        .collect()
}

fn merge_alternatives(
    output: &mut Map<String, Value>,
    alternatives: Vec<Value>,
) -> Result<(), String> {
    if let Some((kind, values)) = enum_alternatives(&alternatives) {
        output.insert("type".to_owned(), Value::String(kind));
        output.insert("enum".to_owned(), Value::Array(values));
        return Ok(());
    }
    if let Some(merged) = merge_object_alternatives(&alternatives)? {
        merge_schema_object(output, merged)?;
        return Ok(());
    }
    output.insert("anyOf".to_owned(), Value::Array(alternatives));
    Ok(())
}

fn enum_alternatives(alternatives: &[Value]) -> Option<(String, Vec<Value>)> {
    let mut kind = None;
    let mut values = Vec::with_capacity(alternatives.len());
    for alternative in alternatives {
        let object = alternative.as_object()?;
        if object.keys().any(|key| key != "type" && key != "enum") {
            return None;
        }
        let current_kind = object.get("type")?.as_str()?;
        if kind.as_deref().is_some_and(|kind| kind != current_kind) {
            return None;
        }
        kind = Some(current_kind.to_owned());
        let enum_values = object.get("enum")?.as_array()?;
        if enum_values.len() != 1 {
            return None;
        }
        values.push(enum_values[0].clone());
    }
    Some((kind?, values))
}

/// 根级 tagged enum 不是所有 OpenAI-compatible 服务都接受。若分支都是 object，
/// 将它们折叠为一个对象：同名 tag 合并为 enum，条件必填退化为各分支 required 的交集。
/// 具体分支仍由 Rust tagged enum 在工具执行前严格校验。
fn merge_object_alternatives(alternatives: &[Value]) -> Result<Option<Map<String, Value>>, String> {
    if alternatives.is_empty() {
        return Ok(None);
    }
    let mut properties = Map::new();
    let mut required: Option<BTreeSet<String>> = None;
    let mut all_closed = true;
    for alternative in alternatives {
        let Some(object) = alternative.as_object() else {
            return Ok(None);
        };
        if object.get("type").and_then(Value::as_str) != Some("object") {
            return Ok(None);
        }
        let Some(branch_properties) = object.get("properties").and_then(Value::as_object) else {
            return Ok(None);
        };
        for (name, schema) in branch_properties {
            match properties.remove(name) {
                None => {
                    properties.insert(name.clone(), schema.clone());
                }
                Some(existing) if existing == *schema => {
                    properties.insert(name.clone(), existing);
                }
                Some(existing) => {
                    properties.insert(name.clone(), merge_property_alternatives(existing, schema));
                }
            }
        }
        let branch_required = object
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        required = Some(match required {
            None => branch_required,
            Some(required) => required.intersection(&branch_required).cloned().collect(),
        });
        all_closed &= object.get("additionalProperties") == Some(&Value::Bool(false));
    }

    let mut merged = Map::from_iter([
        ("type".to_owned(), Value::String("object".to_owned())),
        ("properties".to_owned(), Value::Object(properties)),
    ]);
    if let Some(required) = required
        && !required.is_empty()
    {
        merged.insert(
            "required".to_owned(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
    if all_closed {
        merged.insert("additionalProperties".to_owned(), Value::Bool(false));
    }
    Ok(Some(merged))
}

fn merge_property_alternatives(left: Value, right: &Value) -> Value {
    if let Some((kind, values)) = enum_alternatives(&[left.clone(), right.clone()]) {
        return Value::Object(Map::from_iter([
            ("type".to_owned(), Value::String(kind)),
            ("enum".to_owned(), Value::Array(values)),
        ]));
    }
    Value::Object(Map::from_iter([(
        "anyOf".to_owned(),
        Value::Array(vec![left, right.clone()]),
    )]))
}

fn merge_schema(target: &mut Value, overlay: Value) -> Result<(), String> {
    let Some(target) = target.as_object_mut() else {
        return Err("referenced schema must resolve to an object".to_owned());
    };
    let Value::Object(overlay) = overlay else {
        return Err("schema overlay must be an object".to_owned());
    };
    merge_schema_object(target, overlay)
}

fn merge_schema_object(
    target: &mut Map<String, Value>,
    overlay: Map<String, Value>,
) -> Result<(), String> {
    for (key, value) in overlay {
        if key == "enum"
            && let Some(existing) = target.remove("enum")
        {
            let Some(mut existing) = existing.as_array().cloned() else {
                return Err("enum must be an array".to_owned());
            };
            let Some(values) = value.as_array() else {
                return Err("enum must be an array".to_owned());
            };
            existing.extend(values.iter().cloned());
            target.insert(key, Value::Array(existing));
        } else {
            target.insert(key, value);
        }
    }
    Ok(())
}

fn supported_string_format(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("email" | "hostname" | "ipv4" | "ipv6" | "uuid")
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn complete_dialect_preserves_the_canonical_schema() {
        let schema = representative_schemars_schema();

        let encoded = encode_tool_schema(&schema, ToolSchemaDialect::JsonSchema2020_12)
            .expect("complete schema remains encodable");

        assert_eq!(encoded, schema);
    }

    #[test]
    fn function_subset_lowers_all_schemars_constructs_used_by_standard_tools() {
        let encoded = encode_tool_schema(
            &representative_schemars_schema(),
            ToolSchemaDialect::OpenAiFunctionSubset,
        )
        .expect("schemars schema must lower to the function subset");

        assert_eq!(encoded["type"], json!("object"));
        assert_eq!(encoded["properties"]["action"]["type"], json!("string"));
        assert_eq!(
            encoded["properties"]["action"]["enum"],
            json!(["search", "read"])
        );
        assert_eq!(
            encoded["properties"]["scope"]["enum"],
            json!(["session", "workspace", "global"])
        );
        assert_eq!(encoded["properties"]["sources"]["type"], json!("array"));
        assert_eq!(encoded["properties"]["limit"]["type"], json!("integer"));
        // 分支共同必填字段保留；只在单个分支必填的字段交给 Rust tagged enum 校验。
        assert_eq!(encoded["required"], json!(["action", "limit"]));
        assert_eq!(encoded["additionalProperties"], json!(false));
        assert_function_subset(&encoded);
    }

    #[test]
    fn invalid_local_reference_is_reported_as_configuration_error() {
        let error = encode_tool_schema(
            &json!({"$ref": "#/$defs/missing"}),
            ToolSchemaDialect::OpenAiFunctionSubset,
        )
        .expect_err("missing reference must not be silently dropped");

        assert!(error.contains("does not exist"));
    }

    /// 覆盖当前标准工具由 schemars 生成的方言敏感结构：`$defs/$ref`、根级
    /// tagged union、`const` 枚举、可空数组、默认值和 Rust 整数 format。
    fn representative_schemars_schema() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": {
                "RecallScope": {
                    "oneOf": [
                        {"type": "string", "const": "session"},
                        {"type": "string", "const": "workspace"},
                        {"type": "string", "const": "global"}
                    ]
                }
            },
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "const": "search"},
                        "query": {"type": "string"},
                        "scope": {"$ref": "#/$defs/RecallScope", "default": "session"},
                        "sources": {"type": ["array", "null"], "items": {"type": "string"}},
                        "limit": {"type": "integer", "format": "uint", "minimum": 1}
                    },
                    "required": ["action", "query", "limit"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "const": "read"},
                        "reference": {"type": "string"},
                        "scope": {"$ref": "#/$defs/RecallScope", "default": "session"},
                        "limit": {"type": "integer", "format": "uint", "minimum": 1}
                    },
                    "required": ["action", "reference", "limit"],
                    "additionalProperties": false
                }
            ]
        })
    }

    fn assert_function_subset(value: &Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    assert_function_subset(value);
                }
            }
            Value::Object(object) => {
                for forbidden in [
                    "$schema",
                    "$defs",
                    "definitions",
                    "$ref",
                    "oneOf",
                    "const",
                    "default",
                    "format",
                ] {
                    assert!(
                        !object.contains_key(forbidden),
                        "subset still contains `{forbidden}` in {value}"
                    );
                }
                if let Some(Value::Array(types)) = object.get("type") {
                    assert!(
                        types.iter().all(|kind| kind != "null"),
                        "subset still contains a nullable type in {value}"
                    );
                }
                for value in object.values() {
                    assert_function_subset(value);
                }
            }
            _ => {}
        }
    }
}
