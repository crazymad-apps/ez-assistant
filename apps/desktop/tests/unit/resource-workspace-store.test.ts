import { describe, expect, it } from "vitest";
import {
  CONTEXT_TAB_KEY,
  ResourceWorkspaceStore,
  resourceTabKey,
} from "../../src/features/resource-workspace/ResourceWorkspaceStore";

describe("ResourceWorkspaceStore", () => {
  it("keeps the context tab fixed and cannot close it", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");

    expect(store.tabs.map(resourceTabKey)).toEqual([CONTEXT_TAB_KEY]);
    expect(store.closeTab(CONTEXT_TAB_KEY)).toBe(CONTEXT_TAB_KEY);
    expect(store.tabs.map(resourceTabKey)).toEqual([CONTEXT_TAB_KEY]);
  });

  it("deduplicates stable resource identities while preserving the latest open intent", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openTab({ type: "workspace", scopeKey: "session:one" });
    const locator = { root: { type: "workspace_primary" as const }, relative_path: "main.ts" };
    store.openSessionResource("session:one", "one", locator, "main.ts", 4);
    store.openSessionResource("session:one", "one", locator, "main.ts", 18);

    expect(store.tabs).toHaveLength(3);
    expect(store.active_tab).toMatchObject({ type: "text", line: 18 });
  });

  it("deduplicates attachment and tool resources by their protocol identities", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    const attachment = {
      attachment_id: "attachment-1",
      session_id: "one",
      original_name: "notes.md",
      size_bytes: 12,
      agent_readable_path: "/session/notes.md",
      state: "ready" as const,
      created_at_ms: 1,
    };
    const owner = { type: "main_session" as const, session_id: "one" };
    const file = {
      resource_ref_id: "tool-file-1",
      origin: "workspace_file" as const,
      display_name: "result.json",
      display_path: "result.json",
      size_bytes: 24,
      media_type: "application/json",
      state: "available" as const,
    };

    store.openAttachment("session:one", attachment);
    store.openAttachment("session:one", attachment);
    store.openToolResource("session:one", owner, "message-1", file);
    store.openToolResource("session:one", owner, "message-1", file);

    expect(store.tabs.map(resourceTabKey)).toEqual([
      "context",
      "resource:resource-1",
      "resource:resource-2",
    ]);
  });

  it("keeps browser and terminal instances distinct", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openTab({ type: "browser", browserId: "browser-1" });
    store.openTab({ type: "browser", browserId: "browser-2" });
    store.openTab({ type: "terminal", terminalId: "terminal-1" });

    expect(store.tabs.map(resourceTabKey)).toEqual([
      "context",
      "browser:browser-1",
      "browser:browser-2",
      "terminal:terminal-1",
    ]);
  });

  it("activates the nearest tab on the left after closing the active tab", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openTab({ type: "browser", browserId: "browser-1" });
    store.openTab({ type: "terminal", terminalId: "terminal-1" });

    expect(store.closeTab("terminal:terminal-1")).toBe("browser:browser-1");
    expect(store.active_tab_key).toBe("browser:browser-1");
    expect(store.focused_tab_key).toBe("browser:browser-1");
  });

  it("moves roving focus with wrapping and resets transient tabs on dispose", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    store.openTab({ type: "workspace", scopeKey: "session:one" });
    store.openTab({ type: "browser", browserId: "browser-1" });

    expect(store.moveFocus("next")).toBe("context");
    expect(store.moveFocus("previous")).toBe("browser:browser-1");
    expect(store.moveFocus("first")).toBe("context");
    expect(store.moveFocus("last")).toBe("browser:browser-1");

    store.dispose();
    expect(store.tabs.map(resourceTabKey)).toEqual([CONTEXT_TAB_KEY]);
    expect(store.active_tab_key).toBe(CONTEXT_TAB_KEY);
  });

  it("restores each session tab group and its workspace location", () => {
    const store = new ResourceWorkspaceStore();
    store.selectScope("session:one");
    const private_root = { root: { type: "session_private" as const }, relative_path: "" };
    store.openWorkspace("session:one", private_root);
    store.openTab({ type: "browser", browserId: "browser-1" });

    expect(store.workspace_locations.get("session:one")).toEqual(private_root);
    store.selectScope("session:two");

    expect(store.tabs.map(resourceTabKey)).toEqual(["context"]);
    store.selectScope("session:one");
    expect(store.tabs.map(resourceTabKey)).toEqual(["context", "workspace:session:one", "browser:browser-1"]);
    expect(store.active_tab_key).toBe("browser:browser-1");
    expect(store.workspace_locations.get("session:one")).toEqual(private_root);
  });
});

it("shares a twenty-page LRU across sessions, preserves view indexes and excludes terminals", () => {
  const store = new ResourceWorkspaceStore();
  store.selectScope("session:a");
  store.openWorkspace("session:a");
  const first = store.mounted_pages[0]!;
  store.pageState(first.key).tree = { expanded: [], include_hidden: true, include_generated: false, scroll_top: 160 };
  store.openTab({ type: "terminal", terminalId: "retained" });
  for (let i = 0; i < 20; i++) {
    store.selectScope(`session:${i}`);
    store.openWorkspace(`session:${i}`);
  }
  expect(store.cached_pages.size).toBe(20);
  expect(store.cached_pages.has(first.key)).toBe(false);
  expect(store.mounted_pages.some((page) => page.tab.type === "terminal")).toBe(true);
  store.selectScope("session:a");
  expect(store.active_tab_key).toBe("terminal:retained");
  store.activateTab("workspace:session:a");
  expect(store.cached_pages.has(first.key)).toBe(true);
  expect(store.pageState(first.key).tree?.scroll_top).toBe(160);
  expect(store.cached_pages.size).toBe(20);
  store.closeTab("workspace:session:a");
  expect(store.view_states.has(first.key)).toBe(false);
});

it("transfers a draft group once and ignores late opens after its owner is deleted", () => {
  const store = new ResourceWorkspaceStore();
  store.selectScope("draft:workspace:one");
  store.openWorkspace("draft:workspace:one");
  const owner = store.active_group;
  const page = store.mounted_pages[0]!;
  store.transferDraft("workspace:one", "materialized");
  expect(store.active_group).toBe(owner);
  expect(store.active_tab_key).toBe("workspace:session:materialized");
  expect(store.mounted_pages[0]!.key).toBe(page.key);
  store.selectScope("draft:workspace:one");
  expect(store.active_group.id).not.toBe(owner.id);
  store.transferDraft("workspace:one", "materialized");
  expect(store.groups.get("session:materialized")).toBe(owner);
  store.releaseScope("session:materialized");
  store.openBrowser("example.com", owner);
  store.openLocalResource("session:materialized", { resource_key: "late", display_name: "late.txt", path_segments: ["late.txt"] });
  store.openSessionResource("session:materialized", "materialized", { root: { type: "session_private" }, relative_path: "late" }, "late");
  expect(store.tabs.map(resourceTabKey)).toEqual(["context"]);
  expect(store.browsers.size).toBe(0);
});
