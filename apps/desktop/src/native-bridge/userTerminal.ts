import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import type { SessionResourceLocator } from "../generated/assistant-protocol";

export type TerminalSource = Readonly<
  | { type: "session"; session_id: string; locator: SessionResourceLocator }
  | { type: "workspace"; workspace_id: string }
>;
export type TerminalSize = Readonly<{ cols: number; rows: number }>;
export type TerminalEvent = Readonly<
  | { type: "output"; bytes: number[] }
  | { type: "exited"; code: number }
  | { type: "error"; message: string }
>;
export type CreatedTerminal = Readonly<{ terminal_id: string; directory_name: string }>;

function channel(receive: (event: TerminalEvent) => void): Channel<TerminalEvent> {
  const events = new Channel<TerminalEvent>();
  events.onmessage = receive;
  return events;
}

export function createUserTerminal(source: TerminalSource, size: TerminalSize, receive: (event: TerminalEvent) => void): Promise<CreatedTerminal> {
  if (!isTauri()) return Promise.reject(new Error("请在桌面应用中使用终端。"));
  return invoke("create_user_terminal", { source, size, events: channel(receive) });
}
export function restartUserTerminal(terminal_id: string, size: TerminalSize, receive: (event: TerminalEvent) => void): Promise<void> {
  return invoke("restart_user_terminal", { terminalId: terminal_id, size, events: channel(receive) });
}
export function writeUserTerminal(terminal_id: string, bytes: Uint8Array): Promise<void> {
  return invoke("write_user_terminal", { terminalId: terminal_id, bytes: Array.from(bytes) });
}
export function resizeUserTerminal(terminal_id: string, size: TerminalSize): Promise<void> {
  return invoke("resize_user_terminal", { terminalId: terminal_id, size });
}
export function acknowledgeUserTerminal(terminal_id: string): Promise<void> {
  return invoke("acknowledge_user_terminal", { terminalId: terminal_id });
}
export function closeUserTerminal(terminal_id: string): Promise<void> {
  return invoke("close_user_terminal", { terminalId: terminal_id });
}

export async function shutdownUserTerminals(): Promise<void> {
  if (isTauri()) await invoke("shutdown_user_terminals");
}
export async function resumeUserTerminals(): Promise<void> {
  if (isTauri()) await invoke("resume_user_terminals");
}
