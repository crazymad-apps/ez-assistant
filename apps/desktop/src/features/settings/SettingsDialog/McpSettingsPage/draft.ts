import type { McpServerDraft, McpServerSnapshot, McpServerTransportDraft, McpTransportKind } from "../../../../generated/assistant-protocol";

export function emptyTransport(type: McpTransportKind): McpServerTransportDraft {
  if (type === "streamable_http") return { type, payload: { url: { mode: "replace", value: "" }, headers: {} } };
  return { type, payload: { command: { mode: "replace", value: "" }, args: { mode: "replace", value: [] }, cwd: { mode: "remove" }, environment: {} } };
}

/** 从脱敏快照构建修改意图，不把目标摘要或占位文字作为配置值回写。 */
export function serverDraft(server: McpServerSnapshot | null): McpServerDraft {
  if (!server) return { server_key: "", display_name: "", description: "", enabled: true, transport: emptyTransport("stdio") };
  const keys = server.transport === "stdio" ? server.environment_keys : server.header_keys;
  const secrets = Object.fromEntries(keys.map((key) => [key, { mode: "keep" as const }]));
  const transport: McpServerTransportDraft = server.transport === "stdio"
    ? { type: "stdio", payload: { command: { mode: "keep" }, args: { mode: "keep" }, cwd: { mode: "keep" }, environment: secrets } }
    : { type: "streamable_http", payload: { url: { mode: "keep" }, headers: secrets } };
  return { server_key: server.server_key, display_name: server.display_name, description: server.description,
    enabled: server.enabled, transport, startup_timeout_ms: server.startup_timeout_ms ?? undefined, tool_timeout_ms: server.tool_timeout_ms ?? undefined };
}

export function validateMcpDraft(draft: McpServerDraft): string | null {
  if (!/^[a-z][a-z0-9_-]{0,63}$/u.test(draft.server_key)) return "服务标识必须以小写字母开头，只能包含小写字母、数字、下划线和连字符（最多 64 位）。";
  for (const timeout of [draft.startup_timeout_ms, draft.tool_timeout_ms]) {
    if (timeout !== undefined && (!Number.isSafeInteger(timeout) || timeout <= 0)) return "超时必须是正整数，留空使用全局默认值。";
  }
  if ((draft.startup_timeout_ms ?? 0) > 60000) return "连接超时最多 60000 ms，且受全局连接上限约束。";
  if (draft.transport.type === "stdio") {
    const { command } = draft.transport.payload;
    if (command.mode === "remove" || (command.mode === "replace" && !command.value.trim())) return "请填写要启动的命令。";
  } else {
    const { url } = draft.transport.payload;
    if (url.mode === "remove") return "请填写 MCP 服务 URL。";
    if (url.mode === "replace") {
      try {
        const parsed = new URL(url.value);
        const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(parsed.hostname);
        if ((parsed.protocol !== "https:" && !(parsed.protocol === "http:" && loopback)) || parsed.username || parsed.password || parsed.hash || /\/sse\/?$/u.test(parsed.pathname)) {
          return "URL 需要 HTTPS（本机允许 HTTP），不能包含用户凭据、片段或旧 SSE 地址。";
        }
      } catch { return "请填写有效的 MCP 服务 URL。"; }
    }
  }
  return null;
}
