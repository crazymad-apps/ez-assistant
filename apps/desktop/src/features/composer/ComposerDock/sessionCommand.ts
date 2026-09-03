import type { SessionCommand } from "../../../generated/assistant-protocol";

export type ParsedSessionCommand =
  | Readonly<{ type: "not_command" }>
  | Readonly<{ type: "invalid"; message: string }>
  | Readonly<{ type: "command"; command: SessionCommand }>;

/** 仅完整匹配才转换为结构化命令；保留前缀的错误用法不得落入普通 LLM 输入。 */
export function parseMcpRefreshCommand(text: string): ParsedSessionCommand {
  const trimmed = text.trim();
  if (trimmed === "/mcp") return { type: "not_command" };
  if (!/^\/mcp(?:\s|$)/u.test(trimmed)) return { type: "not_command" };
  const matched = /^\/mcp refresh(?: ([a-z][a-z0-9_-]{0,63}))?$/u.exec(trimmed);
  if (!matched) {
    return { type: "invalid", message: "用法：/mcp refresh 或 /mcp refresh <服务名>" };
  }
  return { type: "command", command: { type: "mcp_refresh", payload: { server: matched[1] } } };
}
