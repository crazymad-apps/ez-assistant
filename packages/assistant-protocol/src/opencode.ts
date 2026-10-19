import type { ProviderConnection } from "./generated/assistant-protocol.ts";

/** 预设只填现有地址字段；显式应用时才覆盖草稿，不表示订阅或推理已验证。 */
export const opencodePresets = [
  { id: "payg", endpoint: "https://opencode.ai/inference/openai/v1", models_path: "/inference/v1/models" },
  { id: "go", endpoint: "https://opencode.ai/inference/go/openai/v1", models_path: "/inference/go/v1/models" },
] as const;

/** 仅派生展示地址；同源绝对列表路径替换全部 path，与 Host/Center 发现规则一致。 */
export function opencodeUrls(connection: Pick<ProviderConnection, "endpoint" | "models_path" | "protocol_preference">): { discovery: string; inference: string } | null {
  try {
    const base = new URL(connection.endpoint);
    if (!["http:", "https:"].includes(base.protocol) || base.username || base.password || base.search || base.hash) return null;
    const path = connection.models_path;
    if (path && (!/^\/[a-zA-Z0-9/_.~-]*$/.test(path) || path.startsWith("//") || path.split("/").some((part) => part === "." || part === ".."))) return null;
    if (connection.protocol_preference === "auto") return null;
    const discovery = new URL(base);
    discovery.pathname = path || `${base.pathname.replace(/\/+$/, "")}/models`;
    const inference = new URL(base);
    inference.pathname = `${base.pathname.replace(/\/+$/, "")}/${connection.protocol_preference === "responses" ? "responses" : "chat/completions"}`;
    return { discovery: discovery.href, inference: inference.href };
  } catch {
    return null;
  }
}
