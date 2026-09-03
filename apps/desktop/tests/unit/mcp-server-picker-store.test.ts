import { describe, expect, it } from "vitest";
import { McpServerPickerStore } from "../../src/features/composer/ComposerDock/McpServerPicker/store";

describe("McpServerPickerStore", () => {
  it("ignores old request failures and responses after a new load or close", async () => {
    const store = new McpServerPickerStore();
    let reject!: (error: Error) => void;
    const old = store.load(() => new Promise((_resolve, fail) => { reject = fail; }));
    const server = { server_key: "github", display_name: "GitHub", description: "Issues", visible_tool_count: 2 };
    await store.load(async () => [server]);
    reject(new Error("旧错误"));
    await old;
    expect(store.servers).toEqual([server]);
    expect(store.error).toBeNull();
    let resolve!: (servers: typeof server[]) => void;
    const pending = store.load(() => new Promise(done => { resolve = done; }));
    store.dispose();
    resolve([server]);
    await pending;
    expect(store.servers).toEqual([]);
    expect(store.loading).toBe(false);
  });
});
