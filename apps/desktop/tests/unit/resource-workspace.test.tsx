import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { ResourceWorkspace } from "../../src/features/resource-workspace/ResourceWorkspace";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(cleanup);

describe("ResourceWorkspace", () => {
  it("renders the fixed context tab and available resource entries", () => {
    renderWorkspace(new RootStore());

    expect(screen.getByRole("tab", { name: "当前上下文" })).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByRole("button", { name: "关闭 当前上下文" })).not.toBeInTheDocument();
    expect(screen.getByRole("tabpanel", { name: "当前上下文" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "新建资源标签" }));
    const menu = screen.getByRole("menu", { name: "新建资源标签" });
    const workspace = within(menu).getByRole("menuitem", { name: /工作空间/ });
    const browser = within(menu).getByRole("menuitem", { name: /浏览器/ });
    const terminal = within(menu).getByRole("menuitem", { name: /终端/ });
    expect(workspace).toBeDisabled();
    expect(browser).toBeEnabled();
    expect(terminal).toBeDisabled();
    expect(workspace).toHaveTextContent("工作空间");
    expect(workspace).not.toHaveTextContent("将在文件浏览里程碑启用");
  });

  it("supports roving focus, activation and deterministic keyboard close", () => {
    const store = new RootStore();
    store.navigation.selectSession("one");
    store.resource_workspace.openTab({ type: "workspace", scopeKey: "session:one" });
    store.resource_workspace.openTab({ type: "browser", browserId: "browser-1" });
    renderWorkspace(store);

    const browser = screen.getByRole("tab", { name: "浏览器" });
    browser.focus();
    fireEvent.keyDown(browser, { key: "ArrowLeft" });
    const workspace = screen.getByRole("tab", { name: "工作空间" });
    expect(workspace).toHaveFocus();
    expect(workspace).toHaveAttribute("aria-selected", "false");

    fireEvent.keyDown(workspace, { key: "Enter" });
    expect(workspace).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(workspace, { key: "Delete" });
    expect(screen.queryByRole("tab", { name: "工作空间" })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "当前上下文" })).toHaveAttribute("aria-selected", "true");
  });

  it("enables the workspace entry for a materialized session", () => {
    const store = new RootStore();
    store.navigation.selectSession("session-1");
    renderWorkspace(store);

    fireEvent.click(screen.getByRole("button", { name: "新建资源标签" }));
    const workspace = within(screen.getByRole("menu", { name: "新建资源标签" }))
      .getByRole("menuitem", { name: "工作空间" });
    expect(workspace).toBeEnabled();
    fireEvent.click(workspace);

    expect(screen.getByRole("tab", { name: "工作空间" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("会话私有目录")).toBeVisible();
  });

  it("keeps tab panels mounted while switching tabs", () => {
    const store = new RootStore();
    store.navigation.selectSession("session-1");
    store.resource_workspace.openWorkspace("session:session-1");
    const view = renderWorkspace(store);
    const tree = view.container.querySelector('[aria-label="工作空间目录"]');
    expect(tree).not.toBeNull();

    fireEvent.click(screen.getByRole("tab", { name: "当前上下文" }));
    expect(tree?.closest('[role="tabpanel"]')).toHaveAttribute("hidden");

    fireEvent.click(screen.getByRole("tab", { name: "工作空间" }));
    expect(view.container.querySelector('[aria-label="工作空间目录"]')).toBe(tree);
  });
});

function renderWorkspace(store: RootStore) {
  return render(
    <RootStoreProvider store={store}>
      <ResourceWorkspace />
      <div id="overlay-root" />
    </RootStoreProvider>,
  );
}

it("keeps session DOM instances until LRU eviction and restores only the selected group", () => {
  const root = new RootStore();
  root.navigation.selectSession("a");
  root.resource_workspace.openWorkspace("session:a");
  const view = renderWorkspace(root);
  const first = screen.getByRole("tree");
  act(() => { root.navigation.selectSession("b"); root.resource_workspace.openWorkspace("session:b"); });
  expect(screen.getByRole("tree")).not.toBe(first);
  expect(first).not.toBeVisible();
  act(() => root.navigation.selectSession("a"));
  expect(screen.getByRole("tree")).toBe(first);
  act(() => {
    for (let i = 0; i < 20; i++) {
      root.navigation.selectSession(`budget-${i}`);
      root.resource_workspace.openWorkspace(`session:budget-${i}`);
    }
  });
  expect(first).not.toBeInTheDocument();
  act(() => root.navigation.selectSession("a"));
  expect(screen.getByRole("tree")).not.toBe(first);
  expect(screen.getAllByRole("tab")).toHaveLength(2);
  view.unmount(); root.dispose();
});
