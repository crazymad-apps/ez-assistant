//! MCP Tool Schema 的有界编译与校验。

use std::{sync::Arc, time::Duration};

use assistant_protocol::{McpDiagnosticCode, McpDiagnosticSnapshot, McpServerKey};
use jsonschema::{Draft, Validator};
use serde_json::Value;
use tokio::sync::Semaphore;

use super::McpToolDefinition;

const MAX_SCHEMA_BYTES: usize = 128 * 1024;
const MAX_SCHEMA_DEPTH: usize = 64;
const MAX_SCHEMA_NODES: usize = 4_096;
const MAX_PATTERN_BYTES: usize = 4 * 1024;
const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_TITLE_BYTES: usize = 128;
const DESCRIPTION_WARNING_BYTES: usize = 4 * 1024;
const MAX_ANNOTATIONS_BYTES: usize = 4 * 1024;
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum McpSchemaFailureKind {
    Invalid,
    Limit,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpSchemaFailure {
    pub(crate) kind: McpSchemaFailureKind,
    pub(crate) message: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpSchemaValidationFailure {
    pub(crate) instance_path: String,
    pub(crate) keyword: String,
}

#[derive(Clone)]
pub(crate) struct CompiledMcpTool {
    pub(crate) definition: McpToolDefinition,
    input: Arc<Validator>,
    output: Option<Arc<Validator>>,
}

impl CompiledMcpTool {
    /// 从已编译的定义派生警告，不保存第二份状态，也不截断模型需要的完整说明。
    /// 仅公开有界工具名与字节数，不把说明正文或连接凭据带入诊断。
    pub(crate) fn description_warning(
        &self,
        server_key: &McpServerKey,
    ) -> Option<McpDiagnosticSnapshot> {
        let bytes = self.definition.description.as_ref()?.len();
        (bytes > DESCRIPTION_WARNING_BYTES).then(|| McpDiagnosticSnapshot {
            server_key: Some(server_key.clone()),
            code: McpDiagnosticCode::ToolDescriptionLong,
            field_path: Some(format!("tools/{}/description", self.definition.name.replace('~', "~0").replace('/', "~1"))),
            message: format!("Tool {:?} description is {bytes} bytes (recommended maximum: {DESCRIPTION_WARNING_BYTES} bytes); retained in full and available", self.definition.name),
        })
    }

    pub(crate) async fn validate_input(
        &self,
        engine: &McpSchemaEngine,
        value: Value,
    ) -> Result<(), McpSchemaValidationFailure> {
        engine.validate(self.input.clone(), value).await
    }

    pub(crate) async fn validate_output(
        &self,
        engine: &McpSchemaEngine,
        value: Value,
    ) -> Result<(), McpSchemaValidationFailure> {
        let Some(output) = &self.output else {
            return Ok(());
        };
        engine.validate(output.clone(), value).await
    }

    pub(crate) fn has_output_schema(&self) -> bool {
        self.output.is_some()
    }
}

/// blocking Schema 工作的全进程有界入口；permit 随 blocking task 一起释放，future
/// 超时不会立即放大后台 CPU 工作量。
pub(crate) struct McpSchemaEngine {
    permits: Arc<Semaphore>,
}

impl McpSchemaEngine {
    pub(crate) fn new(max_blocking_tasks: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_blocking_tasks.max(1))),
        }
    }

    pub(crate) async fn compile_tool(
        &self,
        definition: McpToolDefinition,
    ) -> Result<CompiledMcpTool, McpSchemaFailure> {
        let permit = self.permits.clone().acquire_owned().await.map_err(|_| {
            schema_failure(
                McpSchemaFailureKind::Unavailable,
                "MCP Schema compiler is unavailable",
            )
        })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            compile_tool_blocking(definition)
        })
        .await
        .map_err(|_| {
            schema_failure(
                McpSchemaFailureKind::Unavailable,
                "MCP Schema compiler task failed",
            )
        })?
    }

    async fn validate(
        &self,
        validator: Arc<Validator>,
        value: Value,
    ) -> Result<(), McpSchemaValidationFailure> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| validation_unavailable())?;
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            validator
                .validate(&value)
                .map_err(|error| McpSchemaValidationFailure {
                    instance_path: error.instance_path().to_string(),
                    keyword: error.kind().keyword().to_owned(),
                })
        });
        tokio::time::timeout(VALIDATION_TIMEOUT, work)
            .await
            .map_err(|_| McpSchemaValidationFailure {
                instance_path: String::new(),
                keyword: "timeout".to_owned(),
            })?
            .map_err(|_| validation_unavailable())?
    }
}

fn compile_tool_blocking(
    definition: McpToolDefinition,
) -> Result<CompiledMcpTool, McpSchemaFailure> {
    if definition.name.is_empty() || definition.name.len() > MAX_TOOL_NAME_BYTES {
        return Err(schema_failure(
            McpSchemaFailureKind::Limit,
            "MCP tool name is empty or too large",
        ));
    }
    if definition
        .title
        .as_ref()
        .is_some_and(|value| value.len() > MAX_TITLE_BYTES)
    {
        return Err(schema_failure(
            McpSchemaFailureKind::Limit,
            "MCP tool title exceeds its size limit",
        ));
    }
    if definition
        .annotations
        .as_ref()
        .is_some_and(|value| encoded_len(value) > MAX_ANNOTATIONS_BYTES)
    {
        return Err(schema_failure(
            McpSchemaFailureKind::Limit,
            "MCP tool annotations exceed the size limit",
        ));
    }
    if !definition.input_schema.is_object() {
        return Err(schema_failure(
            McpSchemaFailureKind::Invalid,
            "MCP input Schema must be an object",
        ));
    }
    let input = Arc::new(compile_schema(&definition.input_schema)?);
    let output = definition
        .output_schema
        .as_ref()
        .map(compile_schema)
        .transpose()?
        .map(Arc::new);
    Ok(CompiledMcpTool {
        definition,
        input,
        output,
    })
}

fn compile_schema(schema: &Value) -> Result<Validator, McpSchemaFailure> {
    let encoded = encoded_len(schema);
    if encoded > MAX_SCHEMA_BYTES {
        return Err(schema_failure(
            McpSchemaFailureKind::Limit,
            "MCP Schema exceeds the size limit",
        ));
    }
    let mut nodes = 0usize;
    validate_schema_tree(schema, 1, &mut nodes)?;
    let draft = schema_draft(schema)?;
    jsonschema::options()
        .with_draft(draft)
        .offline()
        .build(schema)
        .map_err(|_| {
            schema_failure(
                McpSchemaFailureKind::Invalid,
                "MCP Schema could not be compiled",
            )
        })
}

fn validate_schema_tree(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), McpSchemaFailure> {
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_SCHEMA_NODES || depth > MAX_SCHEMA_DEPTH {
        return Err(schema_failure(
            McpSchemaFailureKind::Limit,
            "MCP Schema structure exceeds its limit",
        ));
    }
    match value {
        Value::Object(object) => {
            for (name, child) in object {
                if matches!(name.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef") {
                    let Some(reference) = child.as_str() else {
                        return Err(schema_failure(
                            McpSchemaFailureKind::Invalid,
                            "MCP Schema reference must be a string",
                        ));
                    };
                    if !reference.starts_with('#') {
                        return Err(schema_failure(
                            McpSchemaFailureKind::Invalid,
                            "MCP Schema external references are not supported",
                        ));
                    }
                }
                if name == "pattern"
                    && child
                        .as_str()
                        .is_some_and(|pattern| pattern.len() > MAX_PATTERN_BYTES)
                {
                    return Err(schema_failure(
                        McpSchemaFailureKind::Limit,
                        "MCP Schema pattern exceeds the size limit",
                    ));
                }
                if name == "patternProperties"
                    && child.as_object().is_some_and(|patterns| {
                        patterns
                            .keys()
                            .any(|pattern| pattern.len() > MAX_PATTERN_BYTES)
                    })
                {
                    return Err(schema_failure(
                        McpSchemaFailureKind::Limit,
                        "MCP Schema pattern exceeds the size limit",
                    ));
                }
                validate_schema_tree(child, depth.saturating_add(1), nodes)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                validate_schema_tree(child, depth.saturating_add(1), nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn schema_draft(schema: &Value) -> Result<Draft, McpSchemaFailure> {
    let Some(uri) = schema.get("$schema") else {
        return Ok(Draft::Draft202012);
    };
    let Some(uri) = uri.as_str() else {
        return Err(schema_failure(
            McpSchemaFailureKind::Invalid,
            "MCP Schema dialect must be a string",
        ));
    };
    match uri.trim_end_matches('#') {
        "http://json-schema.org/draft-04/schema" => Ok(Draft::Draft4),
        "http://json-schema.org/draft-06/schema" => Ok(Draft::Draft6),
        "http://json-schema.org/draft-07/schema" => Ok(Draft::Draft7),
        "https://json-schema.org/draft/2019-09/schema" => Ok(Draft::Draft201909),
        "https://json-schema.org/draft/2020-12/schema" => Ok(Draft::Draft202012),
        _ => Err(schema_failure(
            McpSchemaFailureKind::Invalid,
            "MCP Schema dialect is not supported",
        )),
    }
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |encoded| encoded.len())
}

fn schema_failure(kind: McpSchemaFailureKind, message: &'static str) -> McpSchemaFailure {
    McpSchemaFailure { kind, message }
}

fn validation_unavailable() -> McpSchemaValidationFailure {
    McpSchemaValidationFailure {
        instance_path: String::new(),
        keyword: "unavailable".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn definition(schema: Value) -> McpToolDefinition {
        McpToolDefinition {
            name: "create_issue".to_owned(),
            title: None,
            description: None,
            input_schema: schema,
            output_schema: None,
            annotations: None,
        }
    }

    #[tokio::test]
    async fn preserves_long_descriptions_and_still_validates_arguments() {
        let engine = McpSchemaEngine::new(1);
        let mut tool = definition(json!({"type":"object", "required":["title"]}));
        let description = "完整说明".repeat(2_000);
        tool.description = Some(description.clone());
        let compiled = engine
            .compile_tool(tool)
            .await
            .expect("long description is allowed");
        assert_eq!(
            compiled.definition.description.as_deref(),
            Some(description.as_str())
        );
        let warning = compiled
            .description_warning(&McpServerKey::new("pencil").expect("key"))
            .expect("warning");
        assert_eq!(warning.code, McpDiagnosticCode::ToolDescriptionLong);
        assert!(warning.message.contains("24000 bytes"));
        assert!(warning.message.contains("4096 bytes"));
        assert!(!warning.message.contains("完整说明"));
        assert!(compiled.validate_input(&engine, json!({})).await.is_err());
        compiled
            .validate_input(&engine, json!({"title":"test"}))
            .await
            .expect("valid input");
    }

    #[tokio::test]
    async fn description_warning_threshold_is_in_bytes_and_other_limits_remain_hard() {
        let key = McpServerKey::new("pencil").expect("key");
        for (description, warn) in [
            ("x".repeat(4096), false),
            ("x".repeat(4097), true),
            ("中".repeat(1366), true),
        ] {
            let mut tool = definition(json!({"type":"object"}));
            tool.description = Some(description);
            let compiled = compile_tool_blocking(tool).expect("allowed");
            assert_eq!(compiled.description_warning(&key).is_some(), warn);
        }
        let mut tool = definition(json!({"type":"object"}));
        tool.title = Some("x".repeat(MAX_TITLE_BYTES + 1));
        assert_eq!(
            compile_tool_blocking(tool).err().expect("title limit").kind,
            McpSchemaFailureKind::Limit
        );
        let mut tool = definition(json!({"type":"object"}));
        tool.annotations = Some(json!({"text":"x".repeat(MAX_ANNOTATIONS_BYTES)}));
        assert_eq!(
            compile_tool_blocking(tool)
                .err()
                .expect("annotation limit")
                .kind,
            McpSchemaFailureKind::Limit
        );
    }

    #[tokio::test]
    async fn compiles_supported_dialects_and_validates_arguments() {
        let engine = McpSchemaEngine::new(2);
        let compiled = engine
            .compile_tool(definition(json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "required": ["title"],
                "properties": {"title": {"type": "string"}}
            })))
            .await
            .expect("compile");
        compiled
            .validate_input(&engine, json!({"title": "bug"}))
            .await
            .expect("valid");
        let failure = compiled
            .validate_input(&engine, json!({"title": 1}))
            .await
            .expect_err("invalid");
        assert_eq!(failure.instance_path, "/title");
        assert_eq!(failure.keyword, "type");
    }

    #[tokio::test]
    async fn rejects_unknown_dialect_external_reference_and_deep_tree() {
        let engine = McpSchemaEngine::new(1);
        for schema in [
            json!({"$schema": "https://example.com/custom", "type": "object"}),
            json!({"type": "object", "properties": {"x": {"$ref": "https://example.com/x"}}}),
        ] {
            let failure = engine
                .compile_tool(definition(schema))
                .await
                .err()
                .expect("invalid");
            assert_eq!(failure.kind, McpSchemaFailureKind::Invalid);
        }

        let mut deep = json!({"type": "object"});
        for _ in 0..MAX_SCHEMA_DEPTH {
            deep = json!({"allOf": [deep]});
        }
        let failure = engine
            .compile_tool(definition(deep))
            .await
            .err()
            .expect("deep");
        assert_eq!(failure.kind, McpSchemaFailureKind::Limit);
    }
}
