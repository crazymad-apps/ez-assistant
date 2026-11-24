import type { ModelFeatureSupport, ModelParameters, ModelTokenLimit, ProviderConnection, ProviderType } from "@ez-assistant/protocol";

export const provider_labels: Record<ProviderType, string> = {
  opencode: "OpenCode", openai: "OpenAI", sub2api: "Sub2API", deepseek: "DeepSeek", dashscope_api: "百炼 API", dashscope_plan: "百炼套餐", moonshot: "Kimi / Moonshot", zhipu: "智谱", vllm: "vLLM", local: "本地兼容服务",
};
export const support_options: readonly { value: ModelFeatureSupport; label: string }[] = [
  { value: "unknown", label: "未知" }, { value: "supported", label: "支持" }, { value: "unsupported", label: "不支持" },
];
export const empty_connection: ProviderConnection = {
  server_history_enabled: false, server_history_support_override: null,
  display_name: "", provider_type: "openai", endpoint: "", protocol_preference: "auto", models_path: "", discovery_format: "openai",
};
export const token_fields = [
  { key: "context_window_tokens", label: "上下文窗口（Token）", required: true },
  { key: "max_output_tokens", label: "运行输出上限（Token）", required: false },
  { key: "max_input_tokens", label: "输入上限（可选）", required: false },
  { key: "reasoning_max_input_tokens", label: "思考模式输入上限（可选）", required: false },
  { key: "reasoning_max_output_tokens", label: "思考模式输出上限（可选）", required: false },
] as const;
export type TokenField = typeof token_fields[number]["key"];
export type TokenDraft = Record<TokenField, string>;
export function tokenDraft(parameters: ModelParameters): TokenDraft {
  return Object.fromEntries(token_fields.map(({ key }) => [key, parameters[key].state === "known" ? String(parameters[key].value) : ""])) as TokenDraft;
}
export function compileTokenDraft(parameters: ModelParameters, draft: TokenDraft): ModelParameters {
  if (parameters.server_auto_compaction_enabled) throw new Error("本版本暂不支持服务端自行触发压缩。");
  if (parameters.server_compaction_enabled && parameters.server_compaction_support !== "supported") throw new Error("请先确认服务端压缩能力为支持。");
  const result = { ...parameters };
  for (const field of token_fields) {
    const value = draft[field.key].trim();
    let limit: ModelTokenLimit = { state: "unknown" };
    if (value) {
      const number = Number(value);
      if (!/^\d+$/.test(value) || !Number.isSafeInteger(number) || number <= 0) throw new Error(`${field.label}必须是正整数。`);
      if (field.key.includes("output") && number > 4_294_967_295) throw new Error(`${field.label}超出支持范围。`);
      limit = { state: "known", value: number };
    } else if (field.required) throw new Error(`请填写${field.label}。`);
    result[field.key] = limit;
  }
  const context = result.context_window_tokens;
  if (context.state === "known") {
    for (const field of token_fields.slice(1)) {
      const value = result[field.key];
      if (value.state === "known" && value.value > context.value) throw new Error(`${field.label}不能超过上下文窗口。`);
    }
  }
  if (result.default_reasoning_effort && !result.reasoning_efforts?.[result.default_reasoning_effort]) throw new Error("默认思考强度必须属于支持的强度集合。");
  return result;
}
// 链接来自本版 docs/resources/v0.25.1-服务商参数文档核查.md；不据文档自动写入模型参数。
export const provider_documents: Partial<Record<ProviderType, string>> = {
  opencode: "https://opencode.ai/v2/docs/console/api/inference/",
  deepseek: "https://api-docs.deepseek.com/api/create-chat-completion/",
  dashscope_api: "https://help.aliyun.com/zh/model-studio/qwen3-8-max",
  dashscope_plan: "https://help.aliyun.com/en/model-studio/token-plan-personal-quick-start",
  moonshot: "https://www.kimi.com/help/kimi-api/api-troubleshooting",
  zhipu: "https://docs.bigmodel.cn/cn/guide/models/text/glm-5.3",
  vllm: "https://docs.vllm.ai/en/latest/configuration/engine_args/#--max-model-len",
};

export function parameterRows(parameters: ModelParameters, draft?: TokenDraft): readonly { label: string; value: string }[] {
  const support = (value: ModelFeatureSupport) => support_options.find((option) => option.value === value)?.label ?? "未知";
  return [
    { label: "压缩方式", value: parameters.server_compaction_enabled ? "服务端原生压缩" : "本地摘要" },
    { label: "服务端自行触发压缩", value: parameters.server_auto_compaction_enabled ? "开启" : "关闭（由 Agent 触发）" },
    { label: "服务端压缩能力", value: support(parameters.server_compaction_support) },
    { label: "输入计量能力", value: support(parameters.input_token_count_support) },
    ...token_fields.map(({ key, label }) => ({ label, value: draft ? draft[key] || "未知" : tokenLabel(parameters[key]) })),
    ...([['streaming', '流式输出'], ['image_input', '图片输入'], ['tool_calls', '工具调用'], ['reasoning', '思考能力']] as const).map(([key, label]) => ({ label, value: (key === "streaming" || key === "tool_calls") && parameters[key] === "unknown" ? "缺省（支持）" : support(parameters[key]) })),
    ...(["auto", "none", "required", "named"] as const).map((key) => ({ label: `工具选择 ${key}`, value: key === "auto" && parameters.tool_choice[key] === "unknown" ? "缺省（支持）" : support(parameters.tool_choice[key]) })),
    { label: "工具图片传递", value: { unknown: "未知", unsupported: "不支持", native_tool_result: "工具结果原生图片", follow_up_user_message: "后续用户消息图片" }[parameters.tool_image_projection] },
    { label: "思考模式", value: { unknown: "未知", unsupported: "不支持", optional: "可开关", always: "始终开启" }[parameters.reasoning_mode] },
    { label: "支持的思考强度", value: parameters.reasoning_efforts === null ? "未知" : Object.entries(parameters.reasoning_efforts).map(([key, value]) => `${key} → ${value}`).join(" / ") || "无强度档位" },
    { label: "默认思考强度", value: parameters.default_reasoning_effort ?? "未设置" },
  ];
}

function tokenLabel(limit: ModelTokenLimit): string {
  if (limit.state === "known") return String(limit.value);
  return limit.state === "invalid" ? "接口值无效" : "未知";
}
/** 比较参数事实，不能将展示文案中的未知与无效视为相同。 */
export function sameParameters(left: ModelParameters, right: ModelParameters): boolean {
  const keys = Object.keys(left) as (keyof ModelParameters)[];
  if (keys.length !== Object.keys(right).length) return false;
  return keys.every((key) => {
    if (key === "tool_choice") return (["auto", "none", "required", "named"] as const).every((choice) => left.tool_choice[choice] === right.tool_choice[choice]);
    if (token_fields.some((field) => field.key === key)) {
      const a = left[key] as ModelTokenLimit;
      const b = right[key] as ModelTokenLimit;
      return a.state === b.state && (a.state !== "known" || (b.state === "known" && a.value === b.value));
    }
    return JSON.stringify(left[key]) === JSON.stringify(right[key]);
  });
}

/** 与 ProviderType.discovery_format 保持一致；列表路径不预填，由 Host 适配器决定默认值。 */
export const provider_defaults: Record<ProviderType, Pick<ProviderConnection, "discovery_format" | "protocol_preference">> = {
  sub2api: { discovery_format: "sub2api", protocol_preference: "auto" },
  opencode: { discovery_format: "openai", protocol_preference: "chat_completions" },
  openai: { discovery_format: "openai", protocol_preference: "auto" },
  deepseek: { discovery_format: "openai", protocol_preference: "auto" },
  dashscope_api: { discovery_format: "dashscope_native", protocol_preference: "chat_completions" },
  dashscope_plan: { discovery_format: "openai", protocol_preference: "chat_completions" },
  moonshot: { discovery_format: "moonshot", protocol_preference: "chat_completions" },
  zhipu: { discovery_format: "openai", protocol_preference: "chat_completions" },
  vllm: { discovery_format: "vllm", protocol_preference: "chat_completions" },
  local: { discovery_format: "openai", protocol_preference: "chat_completions" },
};

export const provider_default_endpoints: Record<ProviderType, string> = {
  openai: "https://api.openai.com/v1",
  deepseek: "https://api.deepseek.com/v1",
  dashscope_api: "https://dashscope.aliyuncs.com/compatible-mode/v1",
  dashscope_plan: "https://coding.dashscope.aliyuncs.com/v1",
  moonshot: "https://api.moonshot.cn/v1",
  zhipu: "https://open.bigmodel.cn/api/paas/v4",
  vllm: "http://127.0.0.1:8000/v1",
  local: "http://127.0.0.1:11434/v1",
  opencode: "",
  sub2api: "",
};

export type ProviderQuickPreset = Readonly<{
  id: string;
  label: string;
  display_name: string;
  provider_type: ProviderType;
  endpoint: string;
  hint: string;
}>;

export const provider_quick_presets: readonly ProviderQuickPreset[] = [
  {
    id: "deepseek",
    label: "DeepSeek",
    display_name: "DeepSeek",
    provider_type: "deepseek",
    endpoint: provider_default_endpoints.deepseek,
    hint: "官方 API · 填写 Key 即用",
  },
  {
    id: "dashscope_api",
    label: "百炼 API",
    display_name: "百炼 API",
    provider_type: "dashscope_api",
    endpoint: provider_default_endpoints.dashscope_api,
    hint: "阿里云百炼按量 Key",
  },
  {
    id: "dashscope_plan",
    label: "百炼套餐",
    display_name: "百炼套餐",
    provider_type: "dashscope_plan",
    endpoint: provider_default_endpoints.dashscope_plan,
    hint: "Coding Plan（sk-sp- 开头）",
  },
  {
    id: "moonshot",
    label: "Kimi / Moonshot",
    display_name: "Kimi / Moonshot",
    provider_type: "moonshot",
    endpoint: provider_default_endpoints.moonshot,
    hint: "月之暗面官方 API",
  },
  {
    id: "zhipu",
    label: "智谱 GLM",
    display_name: "智谱 GLM",
    provider_type: "zhipu",
    endpoint: provider_default_endpoints.zhipu,
    hint: "BigModel v4 官方接口",
  },
  {
    id: "openai",
    label: "OpenAI",
    display_name: "OpenAI",
    provider_type: "openai",
    endpoint: provider_default_endpoints.openai,
    hint: "OpenAI 官方接口",
  },
  {
    id: "local",
    label: "Ollama / 本地",
    display_name: "Ollama 本地模型",
    provider_type: "local",
    endpoint: provider_default_endpoints.local,
    hint: "本机 11434 兼容服务",
  },
];
