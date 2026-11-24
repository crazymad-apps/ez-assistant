import type { ProviderConnection } from "@ez-assistant/protocol";
import { Icon } from "../Icon";
import { CerebrasIcon, KimiIcon, YiIcon } from "./hybridIcons";
import styles from "./index.module.scss";

/**
 * 品牌 SVG 图标静态资源来自 `lobehub/lobe-icons` (`@lobehub/icons-static-svg`, MIT License,
 * https://icons.lobehub.com/)。
 */
export type ProviderIconId =
  | "deepseek"
  | "bailian"
  | "volcengine"
  | "kimi"
  | "zhipu"
  | "siliconcloud"
  | "minimax"
  | "stepfun"
  | "hunyuan"
  | "baiducloud"
  | "spark"
  | "yi"
  | "baichuan"
  | "modelscope"
  | "infinigence"
  | "ppio"
  | "sensenova"
  | "internlm"
  | "openai"
  | "claude"
  | "gemini"
  | "xai"
  | "mistral"
  | "groq"
  | "cohere"
  | "perplexity"
  | "github"
  | "nvidia"
  | "huggingface"
  | "openrouter"
  | "together"
  | "fireworks"
  | "cerebras"
  | "deepinfra"
  | "novita"
  | "sambanova"
  | "hyperbolic"
  | "ai302"
  | "aihubmix"
  | "opencode"
  | "ollama"
  | "lmstudio"
  | "vllm"
  | "custom";

type AssetSpec = Readonly<{
  file: string;
  mode: "color" | "mono";
}>;

const ASSET_MAP: Readonly<Partial<Record<ProviderIconId, AssetSpec>>> = {
  deepseek: { file: "deepseek-color.svg", mode: "color" },
  bailian: { file: "bailian-color.svg", mode: "color" },
  volcengine: { file: "volcengine-color.svg", mode: "color" },
  zhipu: { file: "zhipu-color.svg", mode: "color" },
  siliconcloud: { file: "siliconcloud-color.svg", mode: "color" },
  minimax: { file: "minimax-color.svg", mode: "color" },
  stepfun: { file: "stepfun-color.svg", mode: "color" },
  hunyuan: { file: "hunyuan-color.svg", mode: "color" },
  baiducloud: { file: "baiducloud-color.svg", mode: "color" },
  spark: { file: "spark-color.svg", mode: "color" },
  baichuan: { file: "baichuan-color.svg", mode: "color" },
  modelscope: { file: "modelscope-color.svg", mode: "color" },
  infinigence: { file: "infinigence-color.svg", mode: "color" },
  ppio: { file: "ppio-color.svg", mode: "color" },
  sensenova: { file: "sensenova-color.svg", mode: "color" },
  internlm: { file: "internlm-color.svg", mode: "color" },
  claude: { file: "claude-color.svg", mode: "color" },
  gemini: { file: "gemini-color.svg", mode: "color" },
  mistral: { file: "mistral-color.svg", mode: "color" },
  cohere: { file: "cohere-color.svg", mode: "color" },
  perplexity: { file: "perplexity-color.svg", mode: "color" },
  nvidia: { file: "nvidia-color.svg", mode: "color" },
  huggingface: { file: "huggingface-color.svg", mode: "color" },
  together: { file: "together-color.svg", mode: "color" },
  fireworks: { file: "fireworks-color.svg", mode: "color" },
  deepinfra: { file: "deepinfra-color.svg", mode: "color" },
  novita: { file: "novita-color.svg", mode: "color" },
  sambanova: { file: "sambanova-color.svg", mode: "color" },
  hyperbolic: { file: "hyperbolic-color.svg", mode: "color" },
  ai302: { file: "ai302-color.svg", mode: "color" },
  aihubmix: { file: "aihubmix-color.svg", mode: "color" },
  vllm: { file: "vllm-color.svg", mode: "color" },
  openai: { file: "openai.svg", mode: "mono" },
  xai: { file: "xai.svg", mode: "mono" },
  groq: { file: "groq.svg", mode: "mono" },
  github: { file: "github.svg", mode: "mono" },
  openrouter: { file: "openrouter.svg", mode: "mono" },
  opencode: { file: "opencode.svg", mode: "mono" },
  ollama: { file: "ollama.svg", mode: "mono" },
  lmstudio: { file: "lmstudio.svg", mode: "mono" },
};

const icon_assets = import.meta.glob("./icons/*.svg", {
  eager: true,
  import: "default",
  query: "?url",
}) as Readonly<Record<string, string>>;

function renderIconContent(name: ProviderIconId, size: number) {
  if (name === "kimi") return <KimiIcon size={size} />;
  if (name === "yi") return <YiIcon size={size} />;
  if (name === "cerebras") return <CerebrasIcon size={size} />;
  const spec = ASSET_MAP[name];
  const url = spec ? icon_assets[`./icons/${spec.file}`] : undefined;
  if (!spec || !url) {
    return <Icon name="globe" size={size} />;
  }
  if (spec.mode === "mono") {
    return (
      <span
        aria-hidden="true"
        className={styles.mono_icon}
        style={{
          width: size,
          height: size,
          maskImage: `url("${url}")`,
          WebkitMaskImage: `url("${url}")`,
        }}
      />
    );
  }
  return (
    <img
      alt=""
      aria-hidden="true"
      className={styles.img_icon}
      height={size}
      src={url}
      width={size}
    />
  );
}

export function ProviderIcon(props: Readonly<{
  name: ProviderIconId;
  size?: number;
  className?: string;
}>) {
  const size = props.size ?? 18;
  const classes = props.className ? `${styles.icon_wrap} ${props.className}` : styles.icon_wrap;
  return (
    <span
      aria-hidden="true"
      className={classes}
      data-provider-icon={props.name}
      style={{ width: size, height: size }}
    >
      {renderIconContent(props.name, size)}
    </span>
  );
}

const HOSTNAME_RULES: ReadonlyArray<readonly [string, ProviderIconId]> = [
  ["siliconflow.cn", "siliconcloud"],
  ["volces.com", "volcengine"],
  ["minimaxi.com", "minimax"],
  ["minimax.", "minimax"],
  ["stepfun.", "stepfun"],
  ["hunyuan.", "hunyuan"],
  ["baidubce.com", "baiducloud"],
  ["xf-yun.com", "spark"],
  ["lingyiwanwu.com", "yi"],
  ["baichuan-ai.com", "baichuan"],
  ["modelscope.cn", "modelscope"],
  ["infini-ai.com", "infinigence"],
  ["ppinfra.com", "ppio"],
  ["sensenova.cn", "sensenova"],
  ["intern-ai.org.cn", "internlm"],
  ["anthropic.com", "claude"],
  ["generativelanguage.googleapis.com", "gemini"],
  ["x.ai", "xai"],
  ["mistral.ai", "mistral"],
  ["groq.com", "groq"],
  ["cohere.", "cohere"],
  ["perplexity.ai", "perplexity"],
  ["models.github.ai", "github"],
  ["nvidia.com", "nvidia"],
  ["huggingface.co", "huggingface"],
  ["openrouter.ai", "openrouter"],
  ["together.xyz", "together"],
  ["fireworks.ai", "fireworks"],
  ["cerebras.ai", "cerebras"],
  ["deepinfra.com", "deepinfra"],
  ["novita.ai", "novita"],
  ["sambanova.ai", "sambanova"],
  ["hyperbolic.xyz", "hyperbolic"],
  ["302.ai", "ai302"],
  ["aihubmix.com", "aihubmix"],
  ["api.openai.com", "openai"],
  ["deepseek.com", "deepseek"],
  ["dashscope", "bailian"],
  ["moonshot.", "kimi"],
  ["bigmodel.cn", "zhipu"],
];

function inferByEndpoint(endpoint: string, display_name: string): ProviderIconId {
  const lower_endpoint = endpoint.toLowerCase();
  try {
    const parsed = new URL(endpoint);
    const host = parsed.hostname.toLowerCase();
    for (const [pattern, icon] of HOSTNAME_RULES) {
      if (host === pattern || host.endsWith(`.${pattern}`) || host.includes(pattern)) {
        return icon;
      }
    }
    if (parsed.port === "11434") return "ollama";
    if (parsed.port === "1234") return "lmstudio";
  } catch {
    for (const [pattern, icon] of HOSTNAME_RULES) {
      if (lower_endpoint.includes(pattern)) return icon;
    }
    if (lower_endpoint.includes(":11434")) return "ollama";
    if (lower_endpoint.includes(":1234")) return "lmstudio";
  }
  const lower_name = display_name.toLowerCase();
  if (lower_name.includes("ollama")) return "ollama";
  if (lower_name.includes("lm studio") || lower_name.includes("lmstudio")) return "lmstudio";
  if (lower_name.includes("vllm")) return "vllm";
  return "custom";
}

export function inferModelProviderIcon(
  connection: Pick<ProviderConnection, "provider_type" | "endpoint" | "display_name">,
): ProviderIconId {
  switch (connection.provider_type) {
    case "deepseek":
      return "deepseek";
    case "dashscope_api":
    case "dashscope_plan":
      return "bailian";
    case "moonshot":
      return "kimi";
    case "zhipu":
      return "zhipu";
    case "opencode":
      return "opencode";
    case "vllm":
      return "vllm";
    case "local": {
      const inferred = inferByEndpoint(connection.endpoint, connection.display_name);
      return inferred === "custom" ? "ollama" : inferred;
    }
    case "sub2api":
    case "openai":
      return inferByEndpoint(connection.endpoint, connection.display_name);
  }
}
