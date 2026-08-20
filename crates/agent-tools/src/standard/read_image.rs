//! 单图读取工具壳：复用文件 Read 授权事实，将真实读取与物化委托给 Host 能力。

use agent_types::{ToolName, ToolResultContent, ToolResultPart};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    FileOperation, ReadImageRequest, SharedImageMaterializer, Tool, ToolContext, ToolError,
    ToolExecuteFuture, ToolExecutionMode, ToolResolution,
    standard::fs::{file_resolution, resolve_path},
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadImageInput {
    /// 绝对路径或相对 Session 工作目录的单张图片路径。
    pub path: String,
}

pub struct ReadImageTool {
    materializer: SharedImageMaterializer,
    resolver: crate::SessionPathResolver,
}

impl ReadImageTool {
    pub fn new(
        materializer: SharedImageMaterializer,
        resolver: crate::SessionPathResolver,
    ) -> Self {
        Self {
            materializer,
            resolver,
        }
    }
}

impl Tool for ReadImageTool {
    type Input = ReadImageInput;
    type ResolvedInput = ReadImageRequest;
    type Output = agent_types::ToolImageReference;

    fn name(&self) -> ToolName {
        ToolName::new("read_image").expect("static tool name")
    }

    fn description(&self) -> String {
        "Read one local image so the model can inspect its visual content. The path may be absolute or relative to the session working directory."
            .to_owned()
    }

    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::ParallelEligible
    }

    fn resolve(
        &self,
        input: Self::Input,
    ) -> Result<ToolResolution<Self::ResolvedInput>, ToolError> {
        let path = resolve_path(&self.resolver, &input.path)?;
        file_resolution(
            ReadImageRequest { path: path.clone() },
            FileOperation::Read,
            path,
        )
    }

    fn execute<'a>(
        &'a self,
        input: Self::ResolvedInput,
        context: ToolContext,
    ) -> ToolExecuteFuture<'a, Self::Output> {
        Box::pin(async move {
            self.materializer
                .materialize(input, &context.cancellation)
                .await
                .map_err(|error| ToolError::execution(error.to_string()))
        })
    }

    fn encode_output(output: Self::Output) -> Result<ToolResultContent, String> {
        ToolResultContent::parts(vec![ToolResultPart::image(output)])
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_types::{ToolCall, ToolCallId};

    use super::*;
    use crate::{
        Dispatcher, FileAuthorizationFacts, ImageMaterializationFuture, ImageMaterializer,
        ImageMaterializerError, ResolvedBatchItemRef, ToolRegistry, testutil::block_on,
    };

    struct FixtureMaterializer;

    impl ImageMaterializer for FixtureMaterializer {
        fn materialize<'a>(
            &'a self,
            _request: ReadImageRequest,
            _cancellation: &'a tokio_util::sync::CancellationToken,
        ) -> ImageMaterializationFuture<'a> {
            Box::pin(async move {
                agent_types::ToolImageReference::new(format!("{}.png", "a".repeat(64)), "image/png")
                    .map_err(|_| ImageMaterializerError::Failed)
            })
        }
    }

    fn tool() -> ReadImageTool {
        ReadImageTool::new(
            Arc::new(FixtureMaterializer),
            crate::SessionPathResolver::new(
                crate::AbsolutePath::new("/workspace").expect("absolute workspace"),
            ),
        )
    }

    #[test]
    fn schema_is_strict_single_path_and_resolution_uses_read_facts() {
        let schema = serde_json::to_value(schemars::schema_for!(ReadImageInput)).expect("schema");
        assert_eq!(
            schema
                .pointer("/properties")
                .and_then(serde_json::Value::as_object)
                .expect("properties")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["path"]
        );
        assert_eq!(
            schema.pointer("/additionalProperties"),
            Some(&serde_json::Value::Bool(false))
        );

        let mut registry = ToolRegistry::new();
        registry.register(tool()).expect("register");
        let tools = registry.snapshot();
        let batch = Dispatcher::resolve_batch(
            &tools,
            &[ToolCall {
                id: ToolCallId::new("call-image").expect("call id"),
                name: ToolName::new("read_image").expect("tool name"),
                arguments: serde_json::json!({"path": "chart.png"}),
            }],
        );
        let ResolvedBatchItemRef::Valid(invocation) = batch.get(0).expect("invocation") else {
            panic!("read_image should resolve")
        };
        let facts = invocation
            .facts::<FileAuthorizationFacts>()
            .expect("file facts");
        assert_eq!(facts.operation, FileOperation::Read);
        assert_eq!(
            facts.path.as_path(),
            std::path::Path::new("/workspace/chart.png")
        );
        assert_eq!(
            invocation.execution_mode(),
            ToolExecutionMode::ParallelEligible
        );
    }

    #[test]
    fn execution_returns_an_image_part() {
        let output = block_on(tool().execute(
            ReadImageRequest {
                path: crate::AbsolutePath::new("/workspace/chart.png").expect("path"),
            },
            ToolContext::default(),
        ))
        .expect("materialize");
        let content = ReadImageTool::encode_output(output).expect("encode");
        assert!(matches!(content.as_parts(), [ToolResultPart::Image { .. }]));
    }
}
