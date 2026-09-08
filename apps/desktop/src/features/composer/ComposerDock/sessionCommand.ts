import type { SessionCommand } from "../../../generated/assistant-protocol";

export type ParsedSessionCommand =
  | Readonly<{ type: "not_command" }>
  | Readonly<{ type: "invalid"; message: string }>
  | Readonly<{ type: "command"; command: SessionCommand }>;

/** 完整匹配才发送控制意图；错误的保留指令不作为模型输入。 */
export function parseSessionCommand(text: string): ParsedSessionCommand {
  const trimmed = text.trim();
  if (trimmed === "/mcp" || trimmed === "/skill") return { type: "not_command" };
  if (/^\/skill(?:\s|$)/u.test(trimmed)) {
    if (/^\/skill +refresh$/u.test(trimmed)) return { type: "command", command: { type: "skill_refresh" } };
    return { type: "invalid", message: "用法：/skill refresh" };
  }
  if (!/^\/mcp(?:\s|$)/u.test(trimmed)) return { type: "not_command" };
  const matched = /^\/mcp +refresh(?: +([a-z][a-z0-9_-]{0,63}))?$/u.exec(trimmed);
  if (!matched) return { type: "invalid", message: "用法：/mcp refresh 或 /mcp refresh <服务名>" };
  return { type: "command", command: { type: "mcp_refresh", payload: { server: matched[1] } } };
}
