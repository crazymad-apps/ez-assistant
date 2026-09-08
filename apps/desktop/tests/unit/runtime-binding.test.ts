import { beforeEach, expect, it, vi } from "vitest";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";
const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: bridge.invoke }));
import { activateRuntimeBinding, clearRuntimeBinding, invokeRuntime } from "../../src/native-bridge/runtimeConnection";
const target = (id: string) => ({ binding_id: id, target_kind: "remote" } as RuntimeBootstrap);
beforeEach(() => { vi.clearAllMocks(); clearRuntimeBinding(); });
it("freezes native request headers and discards results after A-B-A", async () => {
  let resolve!: (value: string) => void;
  bridge.invoke.mockReturnValue(new Promise<string>((done) => { resolve = done; }));
  activateRuntimeBinding(target("a-first"));
  const result = invokeRuntime("preview_attachment", { attachmentId: "a-file" });
  activateRuntimeBinding(target("b")); activateRuntimeBinding(target("a-second"));
  expect(bridge.invoke).toHaveBeenCalledWith("preview_attachment", { attachmentId: "a-file" }, { headers: { "x-ez-runtime-binding": "a-first" } });
  resolve("old result"); await expect(result).rejects.toThrow("已丢弃旧操作结果");
});
