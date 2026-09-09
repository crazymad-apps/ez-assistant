import type { DiscoveredModel, ModelParameters, ModelSelection, ProviderSummary } from "../../src/generated/assistant-protocol";

export const modelSelection: ModelSelection = { provider_instance_id: "provider-1", model_id: "fixture" };
export const modelProvider: ProviderSummary = {
  provider_instance_id: "provider-1", has_api_key: true, connection: {
    display_name: "测试服务商", provider_type: "openai", endpoint: "https://example.test/v1",
    protocol_preference: "chat_completions", models_path: "/v1/models", discovery_format: "openai",
  },
};
export function modelParameters(): ModelParameters {
  return {
    context_window_tokens: { state: "known", value: 128000 }, max_output_tokens: { state: "known", value: 4096 },
    max_input_tokens: { state: "unknown" }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
    streaming: "supported", image_input: "unsupported", tool_calls: "unsupported", reasoning: "unsupported",
    tool_choice: { auto: "unsupported", none: "unsupported", required: "unsupported", named: "unsupported" },
    tool_image_projection: "unsupported", reasoning_mode: "unsupported", reasoning_efforts: {}, default_reasoning_effort: null,
  };
}
export function discoveredModel(model_id = "fixture"): DiscoveredModel {
  return { configuration: null, model_id, display_name: null, metadata: modelParameters() };
}
