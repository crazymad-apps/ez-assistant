use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct ResponsesRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub input: Vec<ResponsesInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponsesFunctionTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ResponsesToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ResponsesReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    pub store: bool,
    pub stream: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesInputItem {
    Message {
        role: ResponsesRole,
        content: Vec<ResponsesContent>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: ResponsesFunctionOutput,
    },
    Reasoning {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<Value>,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResponsesRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesContent {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub(super) enum ResponsesFunctionOutput {
    Text(String),
    Parts(Vec<ResponsesContent>),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct ResponsesFunctionTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub strict: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub(super) enum ResponsesToolChoice {
    Mode(ResponsesToolChoiceMode),
    Function {
        #[serde(rename = "type")]
        kind: &'static str,
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResponsesToolChoiceMode {
    None,
    Required,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct ResponsesReasoningConfig {
    pub effort: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ResponsesErrorBody {
    pub error: ResponsesErrorDetail,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ResponsesErrorDetail {
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct ResponsesUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens_details: Option<ResponsesInputTokenDetails>,
    #[serde(default)]
    pub output_tokens_details: Option<ResponsesOutputTokenDetails>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct ResponsesInputTokenDetails {
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct ResponsesOutputTokenDetails {
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
}
