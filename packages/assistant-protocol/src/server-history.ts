import type { ModelFeatureSupport, ProviderConnection } from "./generated/assistant-protocol.ts";

/** 与 Rust ProviderConnection 的纯能力投影一致；自定义接口须显式声明，不由模型名猜测。 */
export function serverHistorySupport(connection: ProviderConnection): ModelFeatureSupport {
  if (connection.provider_type === "opencode") {
    return connection.protocol_preference === "responses" && connection.server_history_support_override !== "unsupported" ? "unknown" : "unsupported";
  }
  const responses = ["openai", "deepseek", "dashscope_api", "dashscope_plan", "moonshot"].includes(connection.provider_type)
    && connection.protocol_preference !== "chat_completions";
  if (!responses) return "unsupported";
  if (connection.server_history_support_override != null) return connection.server_history_support_override;
  const endpoint = connection.endpoint.replace(/\/+$/, "");
  if (connection.provider_type === "openai" && endpoint === "https://api.openai.com/v1") return "supported";
  if (connection.provider_type === "dashscope_api" && dashscopeHistoryEndpoint(endpoint)) return "supported";
  if (["deepseek", "moonshot"].includes(connection.provider_type)) return "unsupported";
  return "unknown";
}

function dashscopeHistoryEndpoint(endpoint: string): boolean {
  return /^https:\/\/(dashscope|dashscope-intl|dashscope-us|cn-hongkong\.dashscope)\.aliyuncs\.com\/compatible-mode\/v1$/.test(endpoint)
    || /^https:\/\/[a-zA-Z0-9-]+\.(cn-beijing|ap-southeast-1|us-east-1|eu-central-1|cn-hongkong|ap-northeast-1)\.maas\.aliyuncs\.com\/compatible-mode\/v1$/.test(endpoint);
}
