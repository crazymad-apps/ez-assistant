import type { SpeechProviderKind } from "@ez-assistant/protocol";
import type { ProviderIconId } from "../../../../components/ProviderIcon";
import type { SelectionOption } from "../../../../components/SelectionPopover";

export type AsrPresetId =
  | "dashscope"
  | "deepseek"
  | "siliconflow"
  | "zhipu"
  | "stepfun"
  | "openai"
  | "groq"
  | "fireworks"
  | "ai302"
  | "custom_openai";

export type TtsPresetId =
  | "dashscope"
  | "deepseek"
  | "siliconflow"
  | "zhipu"
  | "minimax"
  | "stepfun"
  | "openai"
  | "ai302"
  | "custom_openai";

export const CUSTOM_MODEL_SENTINEL = "__custom__";
export const CUSTOM_VOICE_SENTINEL = "__custom__";

export type AsrPresetMeta = Readonly<{
  id: AsrPresetId;
  icon: ProviderIconId;
  label: string;
  description: string;
  hint: string;
  provider: SpeechProviderKind;
  endpoint: string;
  default_model: string;
  models: ReadonlyArray<SelectionOption<string>>;
  requires_custom_endpoint?: boolean;
}>;

export type TtsPresetMeta = Readonly<{
  id: TtsPresetId;
  icon: ProviderIconId;
  label: string;
  description: string;
  hint: string;
  provider: SpeechProviderKind;
  endpoint: string;
  default_model: string;
  default_voice: string;
  models: ReadonlyArray<SelectionOption<string>>;
  requires_custom_endpoint?: boolean;
}>;

export const ASR_PRESETS: Readonly<Record<AsrPresetId, AsrPresetMeta>> = {
  dashscope: {
    id: "dashscope",
    icon: "bailian",
    label: "阿里云百炼 (DashScope)",
    description: "国内直连 · 仅需填写百炼 API Key",
    hint: "仅需填写阿里云百炼 API Key（sk-...），推荐 Qwen3 ASR Flash。",
    provider: "dashscope",
    endpoint: "https://dashscope.aliyuncs.com",
    default_model: "qwen3-asr-flash",
    models: [
      { value: "qwen3-asr-flash", label: "qwen3-asr-flash", description: "千问3 语音识别（推荐）" },
      { value: "paraformer-v2", label: "paraformer-v2", description: "百炼 Paraformer 识别" },
      { value: "fun-asr-realtime", label: "fun-asr-realtime", description: "旧版实时兼容型号" },
    ],
  },
  deepseek: {
    id: "deepseek",
    icon: "deepseek",
    label: "DeepSeek",
    description: "国内直连 · 官方语音转写 (/v1/tools/asr)",
    hint: "仅需填写 DeepSeek API Key（sk-...），自动识别中英日韩等 21 种语言。",
    provider: "deepseek",
    endpoint: "https://api.deepseek.com/v1",
    default_model: "deepseek-asr",
    models: [
      {
        value: "deepseek-asr",
        label: "deepseek-asr",
        description: "DeepSeek 官方语音转写（默认）",
      },
    ],
  },
  siliconflow: {
    id: "siliconflow",
    icon: "siliconcloud",
    label: "硅基流动 (SiliconFlow)",
    description: "国内直连 · SenseVoiceSmall 高性价比",
    hint: "仅需填写硅基流动 API Key（sk-...），默认使用 SenseVoiceSmall。",
    provider: "siliconflow",
    endpoint: "https://api.siliconflow.cn/v1",
    default_model: "FunAudioLLM/SenseVoiceSmall",
    models: [
      {
        value: "FunAudioLLM/SenseVoiceSmall",
        label: "FunAudioLLM/SenseVoiceSmall",
        description: "多语言快速识别（推荐）",
      },
    ],
  },
  zhipu: {
    id: "zhipu",
    icon: "zhipu",
    label: "智谱 BigModel",
    description: "国内直连 · 中文/方言抗噪强",
    hint: "仅需填写智谱开放平台 API Key，支持普通话与多方言抗噪识别。",
    provider: "zhipu",
    endpoint: "https://open.bigmodel.cn/api/paas/v4",
    default_model: "glm-asr-2512",
    models: [
      { value: "glm-asr-2512", label: "glm-asr-2512", description: "智谱新一代 ASR（推荐）" },
    ],
  },
  stepfun: {
    id: "stepfun",
    icon: "stepfun",
    label: "阶跃星辰 (StepFun)",
    description: "国内直连 · Step Audio 高速中文识别",
    hint: "仅需填写阶跃星辰 API Key，兼容 OpenAI Whisper 转写接口。",
    provider: "openai",
    endpoint: "https://api.stepfun.com/v1",
    default_model: "step-asr",
    models: [
      { value: "step-asr", label: "step-asr", description: "阶跃星辰语音识别（推荐）" },
    ],
  },
  openai: {
    id: "openai",
    icon: "openai",
    label: "OpenAI",
    description: "海外 · 官方 Whisper / GPT-4o Transcribe",
    hint: "仅需填写 OpenAI API Key，支持多语种高精度转写。",
    provider: "openai",
    endpoint: "https://api.openai.com/v1",
    default_model: "gpt-4o-mini-transcribe",
    models: [
      { value: "gpt-4o-mini-transcribe", label: "gpt-4o-mini-transcribe", description: "低幻觉快速转写（推荐）" },
      { value: "whisper-1", label: "whisper-1", description: "经典 Whisper 模型" },
    ],
  },
  groq: {
    id: "groq",
    icon: "groq",
    label: "Groq (极速 Whisper)",
    description: "海外 · LPU 极速语音识别",
    hint: "仅需填写 Groq API Key（gsk_...），仅提供 ASR 语音识别。",
    provider: "groq",
    endpoint: "https://api.groq.com/openai/v1",
    default_model: "whisper-large-v3-turbo",
    models: [
      { value: "whisper-large-v3-turbo", label: "whisper-large-v3-turbo", description: "极速 Whisper V3 Turbo（推荐）" },
      { value: "whisper-large-v3", label: "whisper-large-v3", description: "Whisper V3 标准版" },
    ],
  },
  fireworks: {
    id: "fireworks",
    icon: "fireworks",
    label: "Fireworks AI",
    description: "海外 · 极速 Whisper V3 音频端点",
    hint: "仅需填写 Fireworks AI API Key，使用 Fireworks 高吞吐 Whisper V3 Turbo。",
    provider: "openai",
    endpoint: "https://audio-prod.us-virginia-1.direct.fireworks.ai/v1",
    default_model: "whisper-v3-turbo",
    models: [
      { value: "whisper-v3-turbo", label: "whisper-v3-turbo", description: "Fireworks Whisper V3 Turbo（推荐）" },
      { value: "whisper-v3", label: "whisper-v3", description: "Whisper V3 标准版" },
    ],
  },
  ai302: {
    id: "ai302",
    icon: "ai302",
    label: "302.AI",
    description: "聚合网关 · 按量付费多模型 Whisper",
    hint: "仅需填写 302.AI API Key，兼容标准 OpenAI 语音识别接口。",
    provider: "openai",
    endpoint: "https://api.302.ai/v1",
    default_model: "whisper-1",
    models: [
      { value: "whisper-1", label: "whisper-1", description: "Whisper 通用转写（推荐）" },
      { value: "gpt-4o-mini-transcribe", label: "gpt-4o-mini-transcribe", description: "GPT-4o Mini Transcribe" },
    ],
  },
  custom_openai: {
    id: "custom_openai",
    icon: "custom",
    label: "自定义 OpenAI 兼容",
    description: "自建 FunASR / OneAPI / 中转接口",
    hint: "适用于自建 /audio/transcriptions 兼容网关或本地局域网语音服务。",
    provider: "openai",
    endpoint: "",
    default_model: "whisper-1",
    models: [
      { value: "whisper-1", label: "whisper-1" },
      { value: "gpt-4o-mini-transcribe", label: "gpt-4o-mini-transcribe" },
    ],
    requires_custom_endpoint: true,
  },
};

export const TTS_PRESETS: Readonly<Record<TtsPresetId, TtsPresetMeta>> = {
  dashscope: {
    id: "dashscope",
    icon: "bailian",
    label: "阿里云百炼 (DashScope)",
    description: "国内直连 · Qwen3-TTS / CosyVoice，仅需 API Key",
    hint: "仅需填写百炼 API Key（可一键复用语音识别凭据）并挑选音色。",
    provider: "dashscope",
    endpoint: "https://dashscope.aliyuncs.com",
    default_model: "qwen3-tts-flash",
    default_voice: "Cherry",
    models: [
      { value: "qwen3-tts-flash", label: "qwen3-tts-flash", description: "千问3 TTS（音色自然，推荐）" },
      { value: "cosyvoice-v3-flash", label: "cosyvoice-v3-flash", description: "CosyVoice V3" },
      { value: "cosyvoice-v2", label: "cosyvoice-v2", description: "CosyVoice V2" },
    ],
  },
  deepseek: {
    id: "deepseek",
    icon: "deepseek",
    label: "DeepSeek",
    description: "国内直连 · 官方语音合成 (/v1/tools/tts)",
    hint: "仅需填写 DeepSeek API Key（可复用上方语音识别凭据），内置 Stella 女声 / Tide 男声。",
    provider: "deepseek",
    endpoint: "https://api.deepseek.com/v1",
    default_model: "deepseek-tts",
    default_voice: "stella",
    models: [
      {
        value: "deepseek-tts",
        label: "deepseek-tts",
        description: "DeepSeek 官方语音合成（默认）",
      },
    ],
  },
  siliconflow: {
    id: "siliconflow",
    icon: "siliconcloud",
    label: "硅基流动 (SiliconFlow)",
    description: "国内直连 · CosyVoice2 系统 8 音色",
    hint: "仅需填写硅基流动 API Key，后端会自动为系统音色补全模型前缀。",
    provider: "siliconflow",
    endpoint: "https://api.siliconflow.cn/v1",
    default_model: "FunAudioLLM/CosyVoice2-0.5B",
    default_voice: "claire",
    models: [
      {
        value: "FunAudioLLM/CosyVoice2-0.5B",
        label: "FunAudioLLM/CosyVoice2-0.5B",
        description: "CosyVoice2 默认语音合成模型",
      },
    ],
  },
  zhipu: {
    id: "zhipu",
    icon: "zhipu",
    label: "智谱 BigModel",
    description: "国内直连 · GLM-TTS 原生情感合成",
    hint: "仅需填写智谱开放平台 API Key，支持多款系统音色。",
    provider: "zhipu",
    endpoint: "https://open.bigmodel.cn/api/paas/v4",
    default_model: "glm-tts",
    default_voice: "tongtong",
    models: [
      { value: "glm-tts", label: "glm-tts", description: "智谱 GLM-TTS" },
    ],
  },
  minimax: {
    id: "minimax",
    icon: "minimax",
    label: "MiniMax (海螺语音)",
    description: "国内直连 · 新版 API Key 免 GroupId，高拟真音色",
    hint: "仅需填写 MiniMax API Key（sk-cp-... 免 GroupId），直出 16kHz PCM。",
    provider: "minimax",
    endpoint: "https://api.minimaxi.com/v1/t2a_v2",
    default_model: "speech-2.8-turbo",
    default_voice: "female-shaonv",
    models: [
      { value: "speech-2.8-turbo", label: "speech-2.8-turbo", description: "高拟真低延迟（推荐）" },
      { value: "speech-2.6-turbo", label: "speech-2.6-turbo", description: "经典 Turbo" },
      { value: "speech-2.8-hd", label: "speech-2.8-hd", description: "高清音质" },
    ],
  },
  stepfun: {
    id: "stepfun",
    icon: "stepfun",
    label: "阶跃星辰 (StepFun)",
    description: "国内直连 · Step-TTS 中文磁性与温柔音色",
    hint: "仅需填写阶跃星辰 API Key（可复用上方阶跃识别凭据）并挑选音色。",
    provider: "openai",
    endpoint: "https://api.stepfun.com/v1",
    default_model: "step-tts-mini",
    default_voice: "cixingnansheng",
    models: [
      { value: "step-tts-mini", label: "step-tts-mini", description: "低延迟日常语音（推荐）" },
      { value: "step-tts-2", label: "step-tts-2", description: "Step-TTS 2 标准版" },
    ],
  },
  openai: {
    id: "openai",
    icon: "openai",
    label: "OpenAI",
    description: "海外 · GPT-4o Mini TTS / TTS-1",
    hint: "仅需填写 OpenAI API Key 并挑选音色。",
    provider: "openai",
    endpoint: "https://api.openai.com/v1",
    default_model: "gpt-4o-mini-tts",
    default_voice: "alloy",
    models: [
      { value: "gpt-4o-mini-tts", label: "gpt-4o-mini-tts", description: "新一代 TTS（推荐）" },
      { value: "tts-1", label: "tts-1", description: "标准低延迟" },
      { value: "tts-1-hd", label: "tts-1-hd", description: "高清版" },
    ],
  },
  ai302: {
    id: "ai302",
    icon: "ai302",
    label: "302.AI",
    description: "聚合网关 · 按量付费多模型 TTS",
    hint: "仅需填写 302.AI API Key（可复用上方 302.AI 识别凭据）并挑选音色。",
    provider: "openai",
    endpoint: "https://api.302.ai/v1",
    default_model: "gpt-4o-mini-tts",
    default_voice: "alloy",
    models: [
      { value: "gpt-4o-mini-tts", label: "gpt-4o-mini-tts", description: "新一代 TTS（推荐）" },
      { value: "tts-1", label: "tts-1", description: "标准低延迟" },
    ],
  },
  custom_openai: {
    id: "custom_openai",
    icon: "custom",
    label: "自定义 OpenAI 兼容",
    description: "自建 Kokoro / EdgeTTS-OpenAI / 中转接口",
    hint: "适用于自建 /audio/speech 兼容接口或中转服务。",
    provider: "openai",
    endpoint: "",
    default_model: "tts-1",
    default_voice: "alloy",
    models: [
      { value: "tts-1", label: "tts-1" },
      { value: "gpt-4o-mini-tts", label: "gpt-4o-mini-tts" },
    ],
    requires_custom_endpoint: true,
  },
};

export const ASR_PRESET_OPTIONS: ReadonlyArray<SelectionOption<AsrPresetId>> = (
  Object.values(ASR_PRESETS) as AsrPresetMeta[]
).map((item) => ({
  value: item.id,
  label: item.label,
  description: item.description,
}));

export const TTS_PRESET_OPTIONS: ReadonlyArray<SelectionOption<TtsPresetId>> = (
  Object.values(TTS_PRESETS) as TtsPresetMeta[]
).map((item) => ({
  value: item.id,
  label: item.label,
  description: item.description,
}));

const DASHSCOPE_QWEN_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "Cherry", label: "Cherry · 阳光自然女声", description: "中英等多语种友好女声（推荐）" },
  { value: "Serena", label: "Serena · 温柔细腻女声", description: "柔和自然年轻女声" },
  { value: "Ethan", label: "Ethan · 温暖阳光男声", description: "标准普通话、朝气温暖" },
  { value: "Chelsie", label: "Chelsie · 亲和生动女声", description: "富有表现力的女声" },
];

const DASHSCOPE_COSYVOICE_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "longxiaochun", label: "龙小春 (longxiaochun)", description: "知性自然女声" },
  { value: "longanyang", label: "龙安洋 (longanyang)", description: "阳光活力男声" },
  { value: "longanhuan_v3", label: "龙安欢 (longanhuan_v3)", description: "元气女声（支持方言）" },
  { value: "longxiaoxia", label: "龙小夏 (longxiaoxia)", description: "温柔知性女声" },
  { value: "longxiaokai", label: "龙小楷 (longxiaokai)", description: "磁性成熟男声" },
  { value: "longhuhu_v3", label: "龙呼呼 (longhuhu_v3)", description: "天真童声" },
];

const SILICONFLOW_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "claire", label: "Claire · 温柔女声", description: "推荐日常对话女声" },
  { value: "anna", label: "Anna · 沉稳女声" },
  { value: "bella", label: "Bella · 激情女声" },
  { value: "diana", label: "Diana · 欢快女声" },
  { value: "alex", label: "Alex · 沉稳男声" },
  { value: "benjamin", label: "Benjamin · 低沉男声" },
  { value: "charles", label: "Charles · 磁性男声" },
  { value: "david", label: "David · 欢快男声" },
];

const ZHIPU_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "tongtong", label: "彤彤 (tongtong)", description: "默认自然女声" },
  { value: "xiaochen", label: "小陈 (xiaochen)", description: "清亮男声" },
  { value: "chuichui", label: "锤锤 (chuichui)" },
  { value: "jam", label: "Jam (jam)" },
  { value: "kazi", label: "Kazi (kazi)" },
  { value: "douji", label: "豆吉 (douji)" },
  { value: "luodo", label: "罗多 (luodo)" },
];

const MINIMAX_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "female-shaonv", label: "少女 (female-shaonv)", description: "清新自然女声（推荐）" },
  { value: "male-qn-qingse", label: "青涩青年 (male-qn-qingse)", description: "自然年轻男声" },
  { value: "Wise_Woman", label: "成熟女性 (Wise_Woman)" },
  { value: "audiobook_male_1", label: "沉稳男声 (audiobook_male_1)" },
  { value: "English_expressive_narrator", label: "英文旁白 (English_expressive_narrator)" },
  { value: "English_radiant_girl", label: "英文阳光女声 (English_radiant_girl)" },
];

const OPENAI_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "alloy", label: "alloy · 中性平衡" },
  { value: "ash", label: "ash · 清晰沉稳" },
  { value: "ballad", label: "ballad · 温和叙事" },
  { value: "coral", label: "coral · 明亮亲和" },
  { value: "echo", label: "echo · 柔和男声" },
  { value: "fable", label: "fable · 故事表达" },
  { value: "nova", label: "nova · 活力女声" },
  { value: "onyx", label: "onyx · 低沉男声" },
  { value: "sage", label: "sage · 知性平稳" },
  { value: "shimmer", label: "shimmer · 清澈女声" },
  { value: "verse", label: "verse · 朗读表达" },
];

const STEPFUN_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "cixingnansheng", label: "磁性男声 (cixingnansheng)", description: "低沉稳重（推荐）" },
  { value: "wenrounvsheng", label: "温柔女声 (wenrounvsheng)", description: "柔美亲和" },
  { value: "wenrounansheng", label: "温柔男声 (wenrounansheng)", description: "温暖真诚" },
  { value: "wenrougongzi", label: "温柔公子 (wenrougongzi)" },
  { value: "huolinvsheng", label: "活力女声 (huolinvsheng)" },
];

const DEEPSEEK_VOICES: ReadonlyArray<SelectionOption<string>> = [
  { value: "stella", label: "Stella · 自然女声（默认）", description: "支持多语种自然播报" },
  { value: "tide", label: "Tide · 沉稳男声", description: "自然流畅男声" },
];

export function ttsPresetVoices(preset_id: TtsPresetId, model: string): ReadonlyArray<SelectionOption<string>> {
  if (preset_id === "dashscope") {
    return model.trim().toLowerCase().startsWith("qwen")
      ? DASHSCOPE_QWEN_VOICES
      : DASHSCOPE_COSYVOICE_VOICES;
  }
  if (preset_id === "deepseek") return DEEPSEEK_VOICES;
  if (preset_id === "siliconflow") return SILICONFLOW_VOICES;
  if (preset_id === "zhipu") return ZHIPU_VOICES;
  if (preset_id === "minimax") return MINIMAX_VOICES;
  if (preset_id === "stepfun") return STEPFUN_VOICES;
  return OPENAI_VOICES;
}

export function defaultVoiceForModel(preset_id: TtsPresetId, model: string): string {
  const voices = ttsPresetVoices(preset_id, model);
  return voices[0]?.value ?? TTS_PRESETS[preset_id].default_voice;
}

export function detectAsrPreset(provider: SpeechProviderKind, endpoint: string): AsrPresetId {
  if (provider === "dashscope") return "dashscope";
  if (provider === "deepseek") return "deepseek";
  if (provider === "siliconflow") return "siliconflow";
  if (provider === "zhipu") return "zhipu";
  if (provider === "groq") return "groq";
  const normalized = endpoint.trim().replace(/\/+$/, "").toLowerCase();
  if (!normalized || normalized === ASR_PRESETS.openai.endpoint) return "openai";
  if (normalized.includes("api.stepfun.com") || normalized.includes("api.stepfun.ai")) return "stepfun";
  if (normalized.includes("fireworks.ai")) return "fireworks";
  if (normalized.includes("api.302.ai")) return "ai302";
  return "custom_openai";
}

export function detectTtsPreset(provider: SpeechProviderKind, endpoint: string): TtsPresetId {
  if (provider === "dashscope") return "dashscope";
  if (provider === "deepseek") return "deepseek";
  if (provider === "siliconflow") return "siliconflow";
  if (provider === "zhipu") return "zhipu";
  if (provider === "minimax") return "minimax";
  const normalized = endpoint.trim().replace(/\/+$/, "").toLowerCase();
  if (!normalized || normalized === TTS_PRESETS.openai.endpoint) return "openai";
  if (normalized.includes("api.stepfun.com") || normalized.includes("api.stepfun.ai")) return "stepfun";
  if (normalized.includes("api.302.ai")) return "ai302";
  return "custom_openai";
}

export function isSharedSameCloudProvider(asr: AsrPresetId, tts: TtsPresetId): boolean {
  if (asr === "custom_openai" || tts === "custom_openai") return false;
  return (asr as string) === (tts as string);
}
