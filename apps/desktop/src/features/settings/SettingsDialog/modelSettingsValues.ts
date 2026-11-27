import type { ModelFeatureSupport, ModelParameters, ModelTokenLimit, ProviderConnection, ProviderProtocolPreference, ProviderType } from "@ez-assistant/protocol";
import type { ProviderIconId } from "../../../components/ProviderIcon";

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

export type ProviderPresetCategory = "domestic" | "international" | "aggregator" | "local";

export const provider_preset_categories: ReadonlyArray<Readonly<{
  id: ProviderPresetCategory;
  label: string;
}>> = [
  { id: "domestic", label: "国内主流" },
  { id: "international", label: "国际官方" },
  { id: "aggregator", label: "聚合与推理平台" },
  { id: "local", label: "本地部署与自定义" },
];

export type ProviderQuickPreset = Readonly<{
  id: string;
  category: ProviderPresetCategory;
  icon: ProviderIconId;
  label: string;
  display_name: string;
  provider_type: ProviderType;
  endpoint: string;
  models_path?: string;
  protocol_preference?: ProviderProtocolPreference;
  hint: string;
  api_key_placeholder?: string;
  requires_custom_endpoint?: boolean;
  keywords: readonly string[];
}>;

export const provider_quick_presets: readonly ProviderQuickPreset[] = [
  {
    id: "deepseek",
    category: "domestic",
    icon: "deepseek",
    label: "DeepSeek",
    display_name: "DeepSeek",
    provider_type: "deepseek",
    endpoint: provider_default_endpoints.deepseek,
    hint: "DeepSeek 官方接口，填写 API Key 即可直接保存并拉取模型。",
    keywords: ["deepseek", "深度求索", "r1", "v3"],
  },
  {
    id: "dashscope_api",
    category: "domestic",
    icon: "bailian",
    label: "百炼 API",
    display_name: "百炼 API",
    provider_type: "dashscope_api",
    endpoint: provider_default_endpoints.dashscope_api,
    hint: "阿里云百炼按量付费官方接口（Qwen / 通义千问）。",
    keywords: ["dashscope", "aliyun", "bailian", "qwen", "阿里", "百炼", "千问"],
  },
  {
    id: "dashscope_plan",
    category: "domestic",
    icon: "bailian",
    label: "百炼套餐",
    display_name: "百炼套餐",
    provider_type: "dashscope_plan",
    endpoint: provider_default_endpoints.dashscope_plan,
    hint: "阿里云百炼 Coding Plan 专属接入（API Key 通常以 sk-sp- 开头）。",
    api_key_placeholder: "sk-sp-...",
    keywords: ["dashscope", "coding", "plan", "bailian", "百炼套餐", "阿里"],
  },
  {
    id: "volcengine",
    category: "domestic",
    icon: "volcengine",
    label: "火山方舟",
    display_name: "火山方舟 · 豆包",
    provider_type: "openai",
    endpoint: "https://ark.cn-beijing.volces.com/api/v3",
    protocol_preference: "chat_completions",
    hint: "字节跳动火山引擎 Ark 平台，支持豆包及主流开源大模型。",
    keywords: ["volcengine", "ark", "doubao", "bytedance", "火山", "豆包", "字节"],
  },
  {
    id: "moonshot",
    category: "domestic",
    icon: "kimi",
    label: "Kimi / Moonshot",
    display_name: "Kimi / Moonshot",
    provider_type: "moonshot",
    endpoint: provider_default_endpoints.moonshot,
    hint: "月之暗面 Kimi 官方平台，自动读取上下文规格。",
    keywords: ["kimi", "moonshot", "月之暗面"],
  },
  {
    id: "zhipu",
    category: "domestic",
    icon: "zhipu",
    label: "智谱 GLM",
    display_name: "智谱 GLM",
    provider_type: "zhipu",
    endpoint: provider_default_endpoints.zhipu,
    hint: "智谱 BigModel 开放平台 v4 官方接口。",
    keywords: ["zhipu", "glm", "bigmodel", "chatglm", "智谱"],
  },
  {
    id: "siliconflow",
    category: "domestic",
    icon: "siliconcloud",
    label: "硅基流动",
    display_name: "硅基流动 SiliconFlow",
    provider_type: "openai",
    endpoint: "https://api.siliconflow.cn/v1",
    protocol_preference: "chat_completions",
    hint: "国内高速云推理平台，聚合 DeepSeek、Qwen 等开源模型。",
    keywords: ["siliconflow", "siliconcloud", "硅基流动"],
  },
  {
    id: "minimax",
    category: "domestic",
    icon: "minimax",
    label: "MiniMax",
    display_name: "MiniMax",
    provider_type: "openai",
    endpoint: "https://api.minimaxi.com/v1",
    protocol_preference: "chat_completions",
    hint: "MiniMax 国内官方 OpenAI 兼容接口。",
    keywords: ["minimax", "minimaxi", "海螺"],
  },
  {
    id: "stepfun",
    category: "domestic",
    icon: "stepfun",
    label: "阶跃星辰",
    display_name: "阶跃星辰 StepFun",
    provider_type: "openai",
    endpoint: "https://api.stepfun.com/v1",
    protocol_preference: "chat_completions",
    hint: "阶跃星辰 Step 系列多模态与推理模型官方接口。",
    keywords: ["stepfun", "step", "阶跃星辰"],
  },
  {
    id: "hunyuan",
    category: "domestic",
    icon: "hunyuan",
    label: "腾讯混元",
    display_name: "腾讯混元",
    provider_type: "openai",
    endpoint: "https://api.hunyuan.cloud.tencent.com/v1",
    protocol_preference: "chat_completions",
    hint: "腾讯云混元大模型官方 OpenAI 兼容接口。",
    keywords: ["hunyuan", "tencent", "腾讯", "混元"],
  },
  {
    id: "qianfan",
    category: "domestic",
    icon: "baiducloud",
    label: "百度千帆",
    display_name: "百度智能云千帆",
    provider_type: "openai",
    endpoint: "https://qianfan.baidubce.com/v2",
    protocol_preference: "chat_completions",
    hint: "百度千帆 ModelBuilder v2 兼容接口（密钥常以 bce-v3 开头）。",
    api_key_placeholder: "bce-v3/...",
    keywords: ["qianfan", "baidu", "ernie", "wenxin", "百度", "千帆", "文心"],
  },
  {
    id: "spark",
    category: "domestic",
    icon: "spark",
    label: "讯飞星火",
    display_name: "讯飞星火",
    provider_type: "openai",
    endpoint: "https://spark-api-open.xf-yun.com/v1",
    protocol_preference: "chat_completions",
    hint: "科大讯飞星火开放平台 OpenAI 兼容接口。",
    keywords: ["spark", "xfyun", "iflytek", "讯飞", "星火"],
  },
  {
    id: "yi",
    category: "domestic",
    icon: "yi",
    label: "零一万物",
    display_name: "零一万物 Yi",
    provider_type: "openai",
    endpoint: "https://api.lingyiwanwu.com/v1",
    protocol_preference: "chat_completions",
    hint: "零一万物开放平台官方接口。",
    keywords: ["yi", "lingyiwanwu", "01ai", "零一万物"],
  },
  {
    id: "baichuan",
    category: "domestic",
    icon: "baichuan",
    label: "百川智能",
    display_name: "百川智能",
    provider_type: "openai",
    endpoint: "https://api.baichuan-ai.com/v1",
    protocol_preference: "chat_completions",
    hint: "百川大模型官方 OpenAI 兼容接口。",
    keywords: ["baichuan", "百川"],
  },
  {
    id: "modelscope",
    category: "domestic",
    icon: "modelscope",
    label: "魔搭社区",
    display_name: "魔搭 ModelScope",
    provider_type: "openai",
    endpoint: "https://api-inference.modelscope.cn/v1",
    protocol_preference: "chat_completions",
    hint: "阿里云魔搭社区免费与按量开源模型推理接口。",
    keywords: ["modelscope", "魔搭"],
  },
  {
    id: "infinigence",
    category: "domestic",
    icon: "infinigence",
    label: "无问芯穹",
    display_name: "无问芯穹 Infini-AI",
    provider_type: "openai",
    endpoint: "https://cloud.infini-ai.com/maas/v1",
    protocol_preference: "chat_completions",
    hint: "无问芯穹 GenStudio / MaaS 兼容接口。",
    keywords: ["infinigence", "infini", "无问芯穹"],
  },
  {
    id: "ppio",
    category: "domestic",
    icon: "ppio",
    label: "PPIO 派欧云",
    display_name: "PPIO 派欧云",
    provider_type: "openai",
    endpoint: "https://api.ppinfra.com/v3/openai",
    protocol_preference: "chat_completions",
    hint: "PPIO 派欧算力云 OpenAI 兼容推理接口。",
    keywords: ["ppio", "ppinfra", "派欧云"],
  },
  {
    id: "sensenova",
    category: "domestic",
    icon: "sensenova",
    label: "商汤日日新",
    display_name: "商汤日日新 SenseNova",
    provider_type: "openai",
    endpoint: "https://api.sensenova.cn/compatible-mode/v1",
    protocol_preference: "chat_completions",
    hint: "商汤科技日日新开放平台 OpenAI 兼容接口。",
    keywords: ["sensenova", "sensetime", "商汤", "日日新"],
  },
  {
    id: "internlm",
    category: "domestic",
    icon: "internlm",
    label: "书生浦语",
    display_name: "书生浦语 InternLM",
    provider_type: "openai",
    endpoint: "https://chat.intern-ai.org.cn/api/v1",
    protocol_preference: "chat_completions",
    hint: "上海 AI 实验室书生浦语官方开放接口。",
    keywords: ["internlm", "intern", "书生", "浦语"],
  },
  {
    id: "openai",
    category: "international",
    icon: "openai",
    label: "OpenAI",
    display_name: "OpenAI",
    provider_type: "openai",
    endpoint: provider_default_endpoints.openai,
    hint: "OpenAI 官方平台，支持 GPT 与 o 系列及服务端历史续接。",
    keywords: ["openai", "gpt", "o1", "o3", "o4"],
  },
  {
    id: "claude",
    category: "international",
    icon: "claude",
    label: "Anthropic Claude",
    display_name: "Anthropic Claude",
    provider_type: "openai",
    endpoint: "https://api.anthropic.com/v1",
    protocol_preference: "chat_completions",
    hint: "Anthropic 官方 OpenAI 兼容接入端点。",
    keywords: ["anthropic", "claude", "sonnet", "opus"],
  },
  {
    id: "gemini",
    category: "international",
    icon: "gemini",
    label: "Google Gemini",
    display_name: "Google Gemini",
    provider_type: "openai",
    endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
    protocol_preference: "chat_completions",
    hint: "Google AI Studio Gemini OpenAI 兼容端点。",
    keywords: ["google", "gemini", "aistudio"],
  },
  {
    id: "xai",
    category: "international",
    icon: "xai",
    label: "xAI Grok",
    display_name: "xAI Grok",
    provider_type: "openai",
    endpoint: "https://api.x.ai/v1",
    protocol_preference: "chat_completions",
    hint: "xAI Grok 系列官方 API 接口。",
    keywords: ["xai", "grok"],
  },
  {
    id: "mistral",
    category: "international",
    icon: "mistral",
    label: "Mistral AI",
    display_name: "Mistral AI",
    provider_type: "openai",
    endpoint: "https://api.mistral.ai/v1",
    protocol_preference: "chat_completions",
    hint: "Mistral AI 官方平台接口。",
    keywords: ["mistral", "codestral", "mixtral"],
  },
  {
    id: "groq",
    category: "international",
    icon: "groq",
    label: "Groq",
    display_name: "Groq",
    provider_type: "openai",
    endpoint: "https://api.groq.com/openai/v1",
    protocol_preference: "chat_completions",
    hint: "Groq LPU 超低延迟推理接口。",
    api_key_placeholder: "gsk_...",
    keywords: ["groq", "llama"],
  },
  {
    id: "cohere",
    category: "international",
    icon: "cohere",
    label: "Cohere",
    display_name: "Cohere",
    provider_type: "openai",
    endpoint: "https://api.cohere.ai/compatibility/v1",
    protocol_preference: "chat_completions",
    hint: "Cohere Command 系列兼容接口。",
    keywords: ["cohere", "command"],
  },
  {
    id: "perplexity",
    category: "international",
    icon: "perplexity",
    label: "Perplexity",
    display_name: "Perplexity",
    provider_type: "openai",
    endpoint: "https://api.perplexity.ai",
    protocol_preference: "chat_completions",
    hint: "Perplexity Sonar 联网搜索大模型接口。",
    api_key_placeholder: "pplx-...",
    keywords: ["perplexity", "sonar"],
  },
  {
    id: "github",
    category: "international",
    icon: "github",
    label: "GitHub Models",
    display_name: "GitHub Models",
    provider_type: "openai",
    endpoint: "https://models.github.ai/inference",
    protocol_preference: "chat_completions",
    hint: "使用 GitHub PAT 令牌直连 GitHub Models 推理端点。",
    keywords: ["github", "copilot", "models"],
  },
  {
    id: "nvidia",
    category: "international",
    icon: "nvidia",
    label: "NVIDIA NIM",
    display_name: "NVIDIA NIM",
    provider_type: "openai",
    endpoint: "https://integrate.api.nvidia.com/v1",
    protocol_preference: "chat_completions",
    hint: "NVIDIA NIM 托管推理目录接口。",
    api_key_placeholder: "nvapi-...",
    keywords: ["nvidia", "nim", "build"],
  },
  {
    id: "huggingface",
    category: "international",
    icon: "huggingface",
    label: "Hugging Face",
    display_name: "Hugging Face Router",
    provider_type: "openai",
    endpoint: "https://router.huggingface.co/v1",
    protocol_preference: "chat_completions",
    hint: "Hugging Face Inference Providers 统一路由端点。",
    api_key_placeholder: "hf_...",
    keywords: ["huggingface", "hf", "hugging"],
  },
  {
    id: "openrouter",
    category: "aggregator",
    icon: "openrouter",
    label: "OpenRouter",
    display_name: "OpenRouter",
    provider_type: "openai",
    endpoint: "https://openrouter.ai/api/v1",
    protocol_preference: "chat_completions",
    hint: "一个 Key 访问 Claude、GPT、Gemini、DeepSeek 等全平台模型。",
    api_key_placeholder: "sk-or-...",
    keywords: ["openrouter", "聚合", "router"],
  },
  {
    id: "together",
    category: "aggregator",
    icon: "together",
    label: "Together AI",
    display_name: "Together AI",
    provider_type: "openai",
    endpoint: "https://api.together.xyz/v1",
    protocol_preference: "chat_completions",
    hint: "Together AI 开源大模型高性能推理平台。",
    keywords: ["together"],
  },
  {
    id: "fireworks",
    category: "aggregator",
    icon: "fireworks",
    label: "Fireworks AI",
    display_name: "Fireworks AI",
    provider_type: "openai",
    endpoint: "https://api.fireworks.ai/inference/v1",
    protocol_preference: "chat_completions",
    hint: "Fireworks AI 高吞吐低延迟推理接口。",
    keywords: ["fireworks"],
  },
  {
    id: "cerebras",
    category: "aggregator",
    icon: "cerebras",
    label: "Cerebras",
    display_name: "Cerebras",
    provider_type: "openai",
    endpoint: "https://api.cerebras.ai/v1",
    protocol_preference: "chat_completions",
    hint: "Cerebras Wafer-Scale 极速推理引擎。",
    keywords: ["cerebras"],
  },
  {
    id: "deepinfra",
    category: "aggregator",
    icon: "deepinfra",
    label: "DeepInfra",
    display_name: "DeepInfra",
    provider_type: "openai",
    endpoint: "https://api.deepinfra.com/v1/openai",
    protocol_preference: "chat_completions",
    hint: "DeepInfra 按量 Serverless 开源模型平台。",
    keywords: ["deepinfra"],
  },
  {
    id: "novita",
    category: "aggregator",
    icon: "novita",
    label: "Novita AI",
    display_name: "Novita AI",
    provider_type: "openai",
    endpoint: "https://api.novita.ai/v3/openai",
    protocol_preference: "chat_completions",
    hint: "Novita AI Serverless 推理兼容端点。",
    keywords: ["novita"],
  },
  {
    id: "sambanova",
    category: "aggregator",
    icon: "sambanova",
    label: "SambaNova",
    display_name: "SambaNova Cloud",
    provider_type: "openai",
    endpoint: "https://api.sambanova.ai/v1",
    protocol_preference: "chat_completions",
    hint: "SambaNova RDU 高速云端推理接口。",
    keywords: ["sambanova"],
  },
  {
    id: "hyperbolic",
    category: "aggregator",
    icon: "hyperbolic",
    label: "Hyperbolic",
    display_name: "Hyperbolic",
    provider_type: "openai",
    endpoint: "https://api.hyperbolic.xyz/v1",
    protocol_preference: "chat_completions",
    hint: "Hyperbolic 分布式推理服务接口。",
    keywords: ["hyperbolic"],
  },
  {
    id: "ai302",
    category: "aggregator",
    icon: "ai302",
    label: "302.AI",
    display_name: "302.AI",
    provider_type: "openai",
    endpoint: "https://api.302.ai/v1",
    protocol_preference: "chat_completions",
    hint: "302.AI 按需付费全模型聚合平台。",
    keywords: ["302", "ai302", "聚合"],
  },
  {
    id: "aihubmix",
    category: "aggregator",
    icon: "aihubmix",
    label: "AiHubMix",
    display_name: "AiHubMix",
    provider_type: "openai",
    endpoint: "https://aihubmix.com/v1",
    protocol_preference: "chat_completions",
    hint: "AiHubMix 统一 API 路由聚合接口。",
    keywords: ["aihubmix", "聚合"],
  },
  {
    id: "opencode",
    category: "aggregator",
    icon: "opencode",
    label: "OpenCode",
    display_name: "OpenCode",
    provider_type: "opencode",
    endpoint: "https://opencode.ai/inference/openai/v1",
    models_path: "/inference/v1/models",
    protocol_preference: "chat_completions",
    hint: "OpenCode 官方按量与 Go 订阅推理端点。",
    keywords: ["opencode", "zen"],
  },
  {
    id: "ollama",
    category: "local",
    icon: "ollama",
    label: "Ollama",
    display_name: "Ollama 本地模型",
    provider_type: "local",
    endpoint: "http://127.0.0.1:11434/v1",
    hint: "连接本机 Ollama 兼容服务（默认端口 11434，可留空 API Key）。",
    api_key_placeholder: "本地服务可留空或填 ollama",
    keywords: ["ollama", "local", "本地", "11434"],
  },
  {
    id: "lmstudio",
    category: "local",
    icon: "lmstudio",
    label: "LM Studio",
    display_name: "LM Studio 本地模型",
    provider_type: "local",
    endpoint: "http://127.0.0.1:1234/v1",
    hint: "连接本机 LM Studio Local Server（默认端口 1234）。",
    api_key_placeholder: "本地服务可留空或填 lm-studio",
    keywords: ["lm studio", "lmstudio", "local", "本地", "1234"],
  },
  {
    id: "vllm",
    category: "local",
    icon: "vllm",
    label: "vLLM",
    display_name: "vLLM 部署",
    provider_type: "vllm",
    endpoint: provider_default_endpoints.vllm,
    hint: "连接自建 vLLM 服务（默认端口 8000，支持读取 max_model_len）。",
    api_key_placeholder: "如有鉴权请填写，否则留空",
    keywords: ["vllm", "local", "私有部署", "8000"],
  },
  {
    id: "sub2api",
    category: "local",
    icon: "custom",
    label: "Sub2API",
    display_name: "Sub2API",
    provider_type: "sub2api",
    endpoint: "",
    hint: "自定义 Sub2API 增强模型目录服务，请在下方填写私有服务地址。",
    requires_custom_endpoint: true,
    keywords: ["sub2api", "中转", "自定义"],
  },
  {
    id: "custom_openai",
    category: "local",
    icon: "custom",
    label: "自定义 OpenAI 兼容",
    display_name: "自定义 OpenAI 兼容",
    provider_type: "openai",
    endpoint: "",
    protocol_preference: "chat_completions",
    hint: "接入 OneAPI、NewAPI、自建反代或其他标准 OpenAI 兼容端点。",
    requires_custom_endpoint: true,
    keywords: ["custom", "openai", "oneapi", "newapi", "中转", "自定义"],
  },
];
