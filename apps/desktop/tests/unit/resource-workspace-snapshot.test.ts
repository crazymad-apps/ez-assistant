import { afterEach, describe, expect, it, vi } from "vitest";
import { ResourceWorkspaceStore, resourceTabKey } from "../../src/features/resource-workspace/ResourceWorkspaceStore";
import { parseResourceSnapshot } from "../../src/features/resource-workspace/resourceWorkspaceSnapshot";

const native = vi.hoisted(() => ({ register: vi.fn(), createBrowser: vi.fn().mockResolvedValue("new-browser"), createTerminal: vi.fn() }));
vi.mock("../../src/native-bridge/nativeResource", () => ({ registerLocalFileUri: native.register }));
vi.mock("../../src/native-bridge/resourceBrowser", () => ({
  createResourceBrowser: native.createBrowser, closeResourceBrowser: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("../../src/native-bridge/userTerminal", () => ({ createUserTerminal: native.createTerminal }));
const stores: ResourceWorkspaceStore[] = [];
function store() { const value = new ResourceWorkspaceStore(); stores.push(value); return value; }
afterEach(() => { for (const value of stores.splice(0)) value.dispose(); vi.clearAllMocks(); });

describe("right workspace restart snapshot", () => {
  it("round-trips owners/order/selection/view positions while rebuilding handles and deferring terminals", async () => {
    const before = store();
    before.selectScope("session:a");
    const locator = { root: { type: "workspace_primary" as const }, relative_path: "src" };
    before.openWorkspace("session:a", locator);
    before.pageState(before.pageKey(before.active_group, before.active_tab)).tree = {
      expanded: [locator], include_hidden: true, include_generated: false, scroll_top: 120, focus_locator: before.workspace_locations.get("session:a"),
    };
    before.openLocalResource("session:a", { resource_key: "expired-local", display_name: "a.ts", path_segments: ["/", "tmp", "有 空格", "a.ts"] });
    before.pageState(before.pageKey(before.active_group, before.active_tab)).preview = {
      scroll_top: 220, scroll_left: 32, word_wrap: false, editor: null,
    };
    before.openTerminal({ type: "session", session_id: "a", locator }, undefined, true);
    before.openBrowser();
    const browser = [...before.browsers.values()][0]!;
    browser.suspend(); browser.url = "https://example.com/last"; browser.title = "最后页面";
    before.selectScope("session:b");
    before.openToolResource("session:b", { type: "main_session", session_id: "b" }, "message", {
      resource_ref_id: "tool-file", origin: "workspace_file", display_name: "result.json", display_path: "result.json", size_bytes: 24, media_type: "application/json", state: "available",
    });
    const snapshot = parseResourceSnapshot(JSON.parse(JSON.stringify(before.captureSnapshot())))!;
    expect(JSON.stringify(snapshot)).not.toContain("expired-local");
    native.register.mockResolvedValue({ resource_key: "fresh-local", display_name: "a.ts", path_segments: ["/", "tmp", "有 空格", "a.ts"] });
    const after = store(); after.selectScope(snapshot.current_scope_key);
    await after.restoreSnapshot(snapshot);
    expect(after.active_tab).toMatchObject({ type: "text", resource: { display_name: "result.json" } });
    expect(native.register).toHaveBeenCalledWith("file:///tmp/%E6%9C%89%20%E7%A9%BA%E6%A0%BC/a.ts");
    expect(native.createBrowser).not.toHaveBeenCalled();
    expect(native.createTerminal).not.toHaveBeenCalled();
    after.selectScope("session:a");
    expect(after.tabs.map((tab) => tab.type)).toEqual(["context", "workspace", "text", "terminal", "browser"]);
    expect(after.active_tab.type).toBe("browser");
    await vi.waitFor(() => expect(native.createBrowser).toHaveBeenCalledWith("https://example.com/last", expect.any(Function)));
    expect([...after.terminals.values()][0]!.status).toBe("idle");
    const file = after.tabs[2]!;
    expect(resourceTabKey(file)).toBe("resource:fresh-local");
    expect(after.pageState(after.pageKey(after.active_group, file)).preview?.scroll_top).toBe(220);
    const tree = after.pageState(after.pageKey(after.active_group, after.tabs[1]!)).tree!;
    expect(tree).toMatchObject({ expanded: [locator], scroll_top: 120 });
    expect(tree.focus_locator).toBe(after.workspace_locations.get("session:a"));
    after.selectScope("session:b");
    after.openToolResource("session:b", { type: "main_session", session_id: "b" }, "message", {
      resource_ref_id: "tool-file", origin: "workspace_file", display_name: "result.json", display_path: "result.json", size_bytes: 24, media_type: "application/json", state: "available",
    });
    expect(after.tabs).toHaveLength(2);
  });

  it("keeps image transforms through page eviction and restart, rejecting corrupt coordinates", async () => {
    const before = store();
    before.selectScope("session:image");
    before.openSessionResource("session:image", "image", { root: { type: "session_private" }, relative_path: "image.png" }, "image.png");
    const key = before.pageKey(before.active_group, before.active_tab);
    const transform = { scale: 2.5, position_x: -300, position_y: -120 };
    before.pageState(key).preview = { scroll_top: 0, scroll_left: 0, word_wrap: false, editor: null, image: transform };
    for (let i = 0; i < 21; i++) { before.selectScope(`session:other-${i}`); before.openWorkspace(before.current_scope_key); }
    expect(before.cached_pages.has(key)).toBe(false);
    const snapshot = JSON.parse(JSON.stringify(before.captureSnapshot()));
    const after = store();
    await after.restoreSnapshot(parseResourceSnapshot(snapshot)!);
    after.selectScope("session:image");
    expect(after.active_tab.type).toBe("image");
    expect(after.pageState(after.pageKey(after.active_group, after.active_tab)).preview?.image).toEqual(transform);
    const saved = snapshot.groups.find((group: { scope_key: string }) => group.scope_key === "session:image").tabs[1].view_state.preview.image;
    saved.position_x = "invalid";
    expect(() => parseResourceSnapshot(snapshot)).toThrow();
    saved.position_x = 0;
    saved.scale = 0;
    expect(() => parseResourceSnapshot(snapshot)).toThrow();
  });

  it("continues past a missing local file and cancels a late restore after disposal", async () => {
    const snapshot = parseResourceSnapshot({ current_scope_key: "session:a", groups: [{ scope_key: "session:a", active_index: 2, focused_index: 2,
      tabs: [{ page: { type: "context" } }, { page: { type: "resource", name: "gone", source: { type: "local_file", path_segments: ["/", "gone"] }, line: null } }, { page: { type: "workspace" } }],
    }] })!;
    const restored = store(); restored.selectScope("session:a"); native.register.mockRejectedValueOnce(new Error("missing"));
    await restored.restoreSnapshot(snapshot);
    expect(restored.tabs.map((tab) => tab.type)).toEqual(["context", "workspace"]);
    expect(restored.active_tab.type).toBe("workspace");
    expect(restored.browser_error).toContain("1 个本地文件");
    let finish!: (value: unknown) => void;
    native.register.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const late = store(); const pending = late.restoreSnapshot(snapshot); late.dispose();
    finish({ resource_key: "late", display_name: "gone", path_segments: ["/", "gone"] }); await pending;
    expect(late.groups.has("session:a")).toBe(false);
  });

  it("rejects invalid or oversized indexes before opening any native resources", () => {
    expect(parseResourceSnapshot(undefined)).toBeNull();
    expect(() => parseResourceSnapshot({ current_scope_key: "session:a", groups: Array(257).fill({}) })).toThrow();
    expect(() => parseResourceSnapshot({ current_scope_key: "session:a", groups: [{ scope_key: "session:a", active_index: 0, focused_index: 0,
      tabs: [{ page: { type: "browser", url: "file:///etc/passwd", title: "invalid" } }],
    }] })).toThrow();
    expect(native.register).not.toHaveBeenCalled();
  });
});
