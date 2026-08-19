use std::{collections::BTreeSet, path::PathBuf};

use agent_types::{ToolName, ToolResultContent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    InspectImagesRequest, Tool, ToolContext, ToolError, ToolExecuteFuture, ToolResolution,
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
    attachment_root: PathBuf,
}

impl InspectImagesTool {
    pub fn new(inspector: crate::SharedImageInspector, attachment_root: PathBuf) -> Self {
        Self {
            inspector,
            attachment_root,
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
        "Inspect one or more attached images for a specific goal. Use only image paths listed in the conversation; background is optional context.".to_owned()
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
        for path in &input.image_paths {
            let path = PathBuf::from(path);
            let valid = path
                .strip_prefix(&self.attachment_root)
                .ok()
                .is_some_and(|relative| {
                    let components = relative.components().collect::<Vec<_>>();
                    components.len() == 2
                        && components
                            .iter()
                            .all(|component| matches!(component, std::path::Component::Normal(_)))
                });
            if !valid || !seen.insert(path) {
                return Err(ToolError::invalid_input(
                    "image_paths must be unique current-session attachment paths",
                ));
            }
        }
        Ok(ToolResolution::general(ResolvedInspectImagesInput {
            image_paths: input.image_paths,
            goal: input.goal,
            background: input.background.filter(|value| !value.trim().is_empty()),
        }))
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
        Ok(ToolResultContent::Text(output.text))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{ImageInspection, ImageInspectionFuture, ImageInspector, ImageInspectorError};

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
            PathBuf::from("/sessions/s-1/attachments"),
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
    fn resolve_rejects_escape_duplicate_and_blank_goal() {
        let valid = || InspectImagesInput {
            image_paths: vec!["/sessions/s-1/attachments/a/image.png".to_owned()],
            goal: "read the chart".to_owned(),
            background: None,
        };
        assert!(tool().resolve(valid()).is_ok());
        let mut escape = valid();
        escape.image_paths[0] = "/sessions/s-1/attachments/a/../secret.png".to_owned();
        assert!(tool().resolve(escape).is_err());
        let mut blank = valid();
        blank.goal = "  ".to_owned();
        assert!(tool().resolve(blank).is_err());
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
            ToolResultContent::Text("finding".to_owned())
        );
    }

    #[allow(dead_code)]
    fn error_type_is_send(_: ImageInspectorError) {}
}
