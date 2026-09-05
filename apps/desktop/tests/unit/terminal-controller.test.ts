import { beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalController } from "../../src/features/resource-workspace/TerminalController";
import { ResourceWorkspaceStore } from "../../src/features/resource-workspace/ResourceWorkspaceStore";

const fake = vi.hoisted(() => ({
  create: vi.fn(), restart: vi.fn(), close: vi.fn(), ack: vi.fn(), input: vi.fn(), resize: vi.fn(),
  receive: null as null | ((event: { type: "output"; bytes: number[] } | { type: "exited"; code: number } | { type: "error"; message: string }) => void),
  onData: null as null | ((data: string) => void),
  parsed: [] as Array<() => void>,
  write: vi.fn(), dispose: vi.fn(),
}));
vi.mock("../../src/native-bridge/userTerminal", () => ({
  createUserTerminal: fake.create, restartUserTerminal: fake.restart, closeUserTerminal: fake.close,
  acknowledgeUserTerminal: fake.ack, writeUserTerminal: fake.input, resizeUserTerminal: fake.resize,
}));
vi.mock("../../src/features/resource-workspace/terminalEmulator", () => ({
  createTerminalEmulator: async () => ({
    host: document.createElement("div"), fit: { proposeDimensions: () => ({ cols: 80, rows: 24 }) },
    terminal: { cols: 80, rows: 24, dispose: fake.dispose, reset: vi.fn(), resize: vi.fn(), focus: vi.fn(),
      onData: (callback: (data: string) => void) => { fake.onData = callback; },
      onBinary: vi.fn(), attachCustomKeyEventHandler: vi.fn(),
      write: (bytes: Uint8Array, parsed: () => void) => { fake.write(bytes); fake.parsed.push(parsed); },
    },
  }),
}));
const source = { type: "workspace", workspace_id: "workspace-fixture" } as const;
const settle = async () => { await new Promise((resolve) => setTimeout(resolve, 0)); };

beforeEach(() => {
  vi.clearAllMocks(); fake.parsed = []; fake.onData = null;
  fake.create.mockImplementation(async (_source, _size, receive) => { fake.receive = receive; return { terminal_id: "pty-1", directory_name: "fixture" }; });
  fake.close.mockResolvedValue(undefined); fake.ack.mockResolvedValue(undefined); fake.input.mockResolvedValue(undefined);
  fake.restart.mockResolvedValue(undefined);
});

describe("user terminal ownership", () => {
  it("waits for a pending native creation before releasing a closed tab", async () => {
    let created!: (value: { terminal_id: string; directory_name: string }) => void;
    fake.create.mockImplementation(() => new Promise((resolve) => { created = resolve; }));
    const controller = new TerminalController(source, vi.fn());
    await settle();
    const closing = controller.close();
    expect(fake.close).not.toHaveBeenCalled();
    created({ terminal_id: "pending-pty", directory_name: "fixture" });
    await closing;
    expect(fake.close).toHaveBeenCalledWith("pending-pty");
    expect(fake.dispose).toHaveBeenCalledOnce();
    expect(controller.status).toBe("closed");
  });

  it("acks only parsed output even when no pane is mounted", async () => {
    const controller = new TerminalController(source, vi.fn());
    await settle();
    fake.receive?.({ type: "output", bytes: [0xe4, 0xb8] });
    expect(fake.write).toHaveBeenCalledWith(new Uint8Array([0xe4, 0xb8]));
    expect(fake.ack).not.toHaveBeenCalled();
    fake.parsed.shift()?.();
    await settle();
    expect(fake.ack).toHaveBeenCalledWith("pty-1");
    await controller.close();
  });

  it("serializes large UTF-8 paste blocks without splitting or losing bytes", async () => {
    const controller = new TerminalController(source, vi.fn());
    await settle();
    const pasted = "中文".repeat(4000) + "\r";
    fake.onData?.(pasted);
    fake.onData?.("下一条\r");
    await settle();
    const blocks = fake.input.mock.calls.map((call) => call[1] as Uint8Array);
    expect(blocks.every((block) => block.length <= 16384)).toBe(true);
    const combined = new Uint8Array(blocks.reduce((sum, block) => sum + block.length, 0));
    let offset = 0; for (const block of blocks) { combined.set(block, offset); offset += block.length; }
    expect(new TextDecoder().decode(combined)).toBe(pasted + "下一条\r");
    await controller.close();
  });

  it("keeps the native handle after close failure and reuses frozen identity on restart", async () => {
    const controller = new TerminalController(source, vi.fn());
    await settle();
    fake.receive?.({ type: "exited", code: 7 });
    expect(controller.exit_code).toBe(7);
    expect(controller.needs_close_confirmation).toBe(false);
    controller.restart(); await settle();
    expect(fake.restart).toHaveBeenCalledWith("pty-1", { cols: 80, rows: 24 }, expect.any(Function));
    expect(fake.create).toHaveBeenCalledOnce();
    fake.close.mockRejectedValueOnce(new Error("cleanup failed"));
    await expect(controller.close()).rejects.toThrow("cleanup failed");
    expect(fake.dispose).not.toHaveBeenCalled();
    expect(controller.native_id).toBe("pty-1");
    await controller.close();
    expect(controller.status).toBe("closed");
  });
});

describe("terminal tab exit", () => {
  it.each([0, 1, 7])("forwards Ctrl+D and closes only after shell exit (%i) and native cleanup", async (code) => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openWorkspace("session:one");
    store.openTerminal(source);
    await settle();
    fake.onData?.("\u0004");
    await settle();
    expect(fake.input).toHaveBeenCalledWith("pty-1", new Uint8Array([4]));
    expect(store.active_tab.type).toBe("terminal");
    expect(fake.close).not.toHaveBeenCalled();

    let cleaned!: () => void;
    fake.close.mockImplementationOnce(() => new Promise<void>((resolve) => { cleaned = resolve; }));
    fake.receive?.({ type: "exited", code });
    await settle();
    expect(fake.close).toHaveBeenCalledWith("pty-1");
    expect(store.active_tab.type).toBe("terminal");
    cleaned();
    await settle();
    expect(store.terminals.size).toBe(0);
    expect(store.active_tab_key).toBe("workspace:session:one");
    expect(store.focused_tab_key).toBe("workspace:session:one");
    expect(fake.dispose).toHaveBeenCalledOnce();
  });

  it("handles exit before creation resolves without changing the active background replacement", async () => {
    let created!: (value: { terminal_id: string; directory_name: string }) => void;
    fake.create.mockImplementation((_source, _size, receive) => {
      fake.receive = receive;
      return new Promise((resolve) => { created = resolve; });
    });
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openTerminal(source);
    await settle();
    store.selectScope("session:other");
    store.openWorkspace("session:other");
    fake.receive?.({ type: "exited", code: 0 });
    expect(fake.close).not.toHaveBeenCalled();
    created({ terminal_id: "early-pty", directory_name: "fixture" });
    await settle();
    expect(fake.close).toHaveBeenCalledWith("early-pty");
    expect(store.terminals.size).toBe(0);
    expect(store.active_tab_key).toBe("workspace:session:other");
    expect(store.focused_tab_key).toBe("workspace:session:other");
  });

  it("retains communication and cleanup failures for inspection and retry", async () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openTerminal(source);
    await settle();
    const controller = store.terminals.get("terminal-1")!;
    fake.receive?.({ type: "error", message: "channel failed" });
    await settle();
    expect(controller.error).toBe("channel failed");
    expect(fake.close).not.toHaveBeenCalled();
    expect(store.active_tab.type).toBe("terminal");

    controller.restart();
    await settle();
    fake.close.mockRejectedValueOnce(new Error("cleanup failed"));
    fake.receive?.({ type: "exited", code: 0 });
    await settle();
    expect(controller.error).toBe("cleanup failed");
    expect(controller.native_id).toBe("pty-1");
    expect(store.active_tab.type).toBe("terminal");
    expect(fake.dispose).not.toHaveBeenCalled();
    await store.closeTerminalTab("terminal-1");
    expect(store.terminals.size).toBe(0);
  });
});

it("retains a terminal through global cache eviction and closes workspace-sourced PTYs with their session", async () => {
  const store = new ResourceWorkspaceStore();
  store.selectScope("session:owner");
  store.openTerminal(source);
  await settle();
  const terminal = store.terminals.get("terminal-1");
  expect(store.runningTerminalCount("session:owner")).toBe(1);
  for (let i = 0; i < 22; i++) { store.selectScope(`session:${i}`); store.openWorkspace(`session:${i}`); }
  expect(store.terminals.get("terminal-1")).toBe(terminal);
  expect(fake.close).not.toHaveBeenCalled();
  fake.receive?.({ type: "output", bytes: [65] }); fake.parsed.shift()?.();
  await settle();
  expect(fake.ack).toHaveBeenCalledWith("pty-1");
  await store.closeScopeTerminals("session:owner");
  expect(fake.close).toHaveBeenCalledWith("pty-1");
  expect(store.active_tab_key).toBe("workspace:session:21");
  store.releaseScope("session:owner");
  expect(store.terminals.size).toBe(0);
});
