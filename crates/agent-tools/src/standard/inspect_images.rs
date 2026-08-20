use std::collections::BTreeSet;

use agent_types::{ToolName, ToolResultContent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    FileBatchAuthorizationFacts, FileOperation, InspectImagesRequest, SessionPathResolver, Tool,
    ToolContext, ToolError, ToolExecuteFuture, ToolResolution, standard::fs::resolve_path,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectImagesInput {
    #[schemars(length(min = 1))]
    pub image_paths: Vec<String>,
    #[schemars(length(min = 1))]
    pub goal: String,
    pub background: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolvedInspectImagesInput {
    pub image_paths: Vec<String>,
    pub goal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InspectImagesOutput {
    text: String,
    model_key: String,
    elapsed_ms: u64,
    usage: Option<agent_types::TokenUsage>,
}

pub struct InspectImagesTool {
    inspector: crate::SharedImageInspector,
    resolver: SessionPathResolver,
}

impl InspectImagesTool {
    pub fn new(inspector: crate::SharedImageInspector, resolver: SessionPathResolver) -> Self {
        Self {
            inspector,
            resolver,
        }
    }
}

impl Tool for InspectImagesTool {
    type Input = InspectImagesInput;
    type ResolvedInput = ResolvedInspectImagesInput;
    type Output = InspectImagesOutput;

    fn name(&self) -> ToolName {
        ToolName::new("inspect_images").expect("static tool name")
    }

    fn description(&self) -> String {
        "Inspect one or more local images for a specific goal. Paths may be absolute or relative to the session working directory; background is optional context."
            .to_owned()
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        if input.goal.trim().is_empty() {
            return Err(ToolError::invalid_input("goal must not be blank"));
        }
        if input.image_paths.is_empty() || input.image_paths.len() > 10 {
            return Err(ToolError::invalid_input(
                "image_paths must contain between 1 and 10 entries",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut paths = Vec::with_capacity(input.image_paths.len());
        for path in input.image_paths {
            let path = resolve_path(&self.resolver, &path)?;
            if !seen.insert(path.clone()) {
                return Err(ToolError::invalid_input(
                    "image_paths must resolve to unique paths",
                ));
            }
            paths.push(path);
        }
        let resolved = ResolvedInspectImagesInput {
            image_paths: paths.iter().map(|path| path.as_str().to_owned()).collect(),
            goal: input.goal,
            background: input.background.filter(|value| !value.trim().is_empty()),
        };
        let mut semantic_arguments = serde_json::to_value(&resolved)
            .map_err(|error| ToolError::invalid_input(error.to_string()))?;
        let serde_json::Value::Object(arguments) = &mut semantic_arguments else {
            return Err(ToolError::invalid_input(
                "resolved inspect_images input must serialize as an object",
            ));
        };
        arguments.insert(
            "operation".to_owned(),
            serde_json::to_value(FileOperation::Read)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?,
        );
        Ok(ToolResolution::with_facts(
            resolved,
            FileBatchAuthorizationFacts {
                operation: FileOperation::Read,
                paths,
            },
            semantic_arguments,
        ))
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            let result = self
                .inspector
                .inspect(
                    InspectImagesRequest {
                        image_paths: input.image_paths,
                        goal: input.goal,
                        background: input.background,
                    },
                    &context.cancellation,
                )
                .await
                .map_err(|error| ToolError::execution(error.to_string()))?;
            Ok(InspectImagesOutput {
                text: result.text,
                model_key: result.model_key,
                elapsed_ms: result.elapsed_ms,
                usage: result.usage,
            })
        })
    }

    fn execution_metadata(output: &Self::Output) -> Option<agent_types::ToolExecutionMetadata> {
        Some(agent_types::ToolExecutionMetadata {
            model_key: Some(output.model_key.clone()),
            elapsed_ms: Some(output.elapsed_ms),
            usage: output.usage.clone(),
        })
    }

    fn encode_output(output: Self::Output) -> Result<ToolResultContent, String> {
        Ok(ToolResultContent::text(output.text))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_types::{ToolCall, ToolCallId};

    use super::*;
    use crate::{
        AbsolutePath, Dispatcher, ImageInspection, ImageInspectionFuture, ImageInspector,
        ImageInspectorError, ResolvedBatchItemRef, ToolRegistry,
    };

    struct FixtureInspector;

    impl ImageInspector for FixtureInspector {
        fn inspect<'a>(
            &'a self,
            request: InspectImagesRequest,
            _cancellation: &'a tokio_util::sync::CancellationToken,
        ) -> ImageInspectionFuture<'a> {
            Box::pin(async move {
                Ok(ImageInspection {
                    text: format!("inspected {}", request.goal),
                    model_key: "vision".to_owned(),
                    elapsed_ms: 12,
                    usage: None,
                })
            })
        }
    }

    fn tool() -> InspectImagesTool {
        InspectImagesTool::new(
            Arc::new(FixtureInspector),
            SessionPathResolver::new(AbsolutePath::new("/workspace").expect("workspace")),
        )
    }

    #[test]
    fn input_contract_has_only_images_goal_and_optional_background() {
        let schema =
            serde_json::to_value(schemars::schema_for!(InspectImagesInput)).expect("schema");
        let properties = schema
            .pointer("/properties")
            .and_then(serde_json::Value::as_object)
            .expect("properties");
        assert_eq!(
            properties.keys().cloned().collect::<Vec<_>>(),
            vec!["background", "goal", "image_paths"]
        );
        assert_eq!(
            schema.pointer("/additionalProperties"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn resolve_accepts_workspace_absolute_and_relative_paths_with_batch_read_facts() {
        let valid = || InspectImagesInput {
            image_paths: vec!["chart.png".to_owned(), "/tmp/reference.png".to_owned()],
            goal: "read the chart".to_owned(),
            background: None,
        };
        let resolution = tool().resolve(valid()).expect("resolve paths");
        let resolved = resolution.into_input();
        assert_eq!(
            resolved.image_paths,
            vec!["/workspace/chart.png", "/tmp/reference.png"]
        );

        let mut blank = valid();
        blank.goal = "  ".to_owned();
        assert!(tool().resolve(blank).is_err());

        let duplicate = InspectImagesInput {
            image_paths: vec!["chart.png".to_owned(), "/workspace/./chart.png".to_owned()],
            goal: "compare".to_owned(),
            background: None,
        };
        assert!(tool().resolve(duplicate).is_err());

        let mut registry = ToolRegistry::new();
        registry.register(tool()).expect("register");
        let tools = registry.snapshot();
        let batch = Dispatcher::resolve_batch(
            &tools,
            &[ToolCall {
                id: ToolCallId::new("call-inspect").expect("call id"),
                name: ToolName::new("inspect_images").expect("tool name"),
                arguments: serde_json::json!({
                    "image_paths": ["chart.png", "/tmp/reference.png"],
                    "goal": "compare"
                }),
            }],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("invocation") else {
            panic!("inspect_images should resolve")
        };
        let facts = invocation
            .facts::<FileBatchAuthorizationFacts>()
            .expect("batch file facts");
        assert_eq!(facts.operation, FileOperation::Read);
        assert_eq!(
            facts
                .paths
                .iter()
                .map(AbsolutePath::as_str)
                .collect::<Vec<_>>(),
            vec!["/workspace/chart.png", "/tmp/reference.png"]
        );
    }

    #[test]
    fn output_is_direct_text_not_structured_json() {
        let output = InspectImagesOutput {
            text: "finding".to_owned(),
            model_key: "vision".to_owned(),
            elapsed_ms: 12,
            usage: None,
        };
        assert_eq!(
            InspectImagesTool::execution_metadata(&output).and_then(|metadata| metadata.model_key),
            Some("vision".to_owned())
        );
        assert_eq!(
            InspectImagesTool::encode_output(output).expect("output"),
            ToolResultContent::text("finding".to_owned())
        );
    }

    #[allow(dead_code)]
    fn error_type_is_send(_: ImageInspectorError) {}
}
