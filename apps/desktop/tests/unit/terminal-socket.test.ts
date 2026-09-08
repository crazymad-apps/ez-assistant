import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { TerminalSocket } from "../../src/runtime-client/TerminalSocket";

class SocketFixture {
  static OPEN = 1;
  static connections: SocketFixture[] = [];
  readyState = 0;
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: unknown[] = [];
  constructor(readonly url: URL) { SocketFixture.connections.push(this); }
  send(value: unknown) { this.sent.push(value); }
  open() { this.readyState = 1; this.onopen?.(); }
  message(value: unknown) { this.onmessage?.({ data: JSON.stringify(value) }); }
  close() { this.readyState = 3; this.onclose?.(); }
}
const source = { type: "workspace", workspace_id: "workspace" } as const;
beforeEach(() => { SocketFixture.connections = []; vi.stubGlobal("WebSocket", SocketFixture); vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it("uses matching WSS with the credential only in the first frame; input awaits Host acknowledgement", async () => {
  const owner = new AbortController();
  const terminal = new TerminalSocket("https://runtime.test:7240", "private-token", source, { cols: 80, rows: 24 }, vi.fn(), owner.signal);
  const socket = SocketFixture.connections[0]!;
  expect(socket.url.toString()).toBe("wss://runtime.test:7240/user-terminals/socket");
  socket.open();
  expect(JSON.parse(socket.sent[0] as string)).toMatchObject({ type: "open", bearer: "private-token", source });
  socket.message({ type: "created", terminal_id: "owned", directory_name: "workspace" });
  await terminal.created;
  let written = false;
  const input = terminal.write(new TextEncoder().encode("中文")).then(() => { written = true; });
  await Promise.resolve();
  expect(written).toBe(false);
  await expect(terminal.write(new Uint8Array([1]))).rejects.toThrow("不能接收输入");
  socket.message({ type: "input_ack" });
  await input;
  expect(written).toBe(true);
  owner.abort();
});

it("disposing one client closes only its socket and never reconnects after a drop", async () => {
  const first = new AbortController();
  const second = new AbortController();
  const event = vi.fn();
  const a = new TerminalSocket("http://one.test", "one", source, { cols: 80, rows: 24 }, event, first.signal);
  const b = new TerminalSocket("http://two.test", "two", source, { cols: 80, rows: 24 }, event, second.signal);
  for (const [index, socket] of SocketFixture.connections.entries()) {
    socket.open(); socket.message({ type: "created", terminal_id: String(index), directory_name: "workspace" });
  }
  await Promise.all([a.created, b.created]);
  first.abort();
  expect(SocketFixture.connections.map((socket) => socket.readyState)).toEqual([3, 1]);
  SocketFixture.connections[1]!.close();
  expect(event).toHaveBeenCalledWith({ type: "error", message: "终端已断开，请重新创建。" });
  await vi.advanceTimersByTimeAsync(60_000);
  expect(SocketFixture.connections).toHaveLength(2);
});

it("an aborted pending connection cannot send credentials or create a terminal later", async () => {
  const owner = new AbortController();
  const terminal = new TerminalSocket("http://runtime.test", "private-token", source, { cols: 80, rows: 24 }, vi.fn(), owner.signal);
  const failed = expect(terminal.created).rejects.toThrow("已关闭");
  owner.abort();
  SocketFixture.connections[0]!.open();
  await failed;
  expect(SocketFixture.connections[0]!.sent).toEqual([]);
});

it("does not report successful cleanup when Host reports failure without Closed", async () => {
  const terminal = new TerminalSocket("http://runtime.test", "token", source, { cols: 80, rows: 24 }, vi.fn(), new AbortController().signal);
  const socket = SocketFixture.connections[0]!;
  socket.open(); socket.message({ type: "created", terminal_id: "owned", directory_name: "workspace" });
  await terminal.created;
  const closed = expect(terminal.close()).rejects.toThrow("终端清理失败");
  socket.message({ type: "error", message: "终端清理失败" });
  socket.close();
  await closed;
});
