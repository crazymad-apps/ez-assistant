import { AppearanceProvider } from "../../src/stores/AppearanceContext";
import { AppearanceStore } from "../../src/stores/AppearanceStore";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { runInAction } from "mobx";
import * as nativeMenu from "../../src/native-bridge/resourceMenu";
import { DevicePicker } from "../../src/features/runtime-access/DevicePicker";
import { ResourceWorkspace } from "../../src/features/resource-workspace/ResourceWorkspace";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("ResourceWorkspace", () => {
  it("renders the fixed context tab and hides the desktop browser entry on Web", () => {
    renderWorkspace(new RootStore());

    expect(screen.getByRole("tab", { name: "当前上下文" })).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByRole("button", { name: "关闭 当前上下文" })).not.toBeInTheDocument();
    expect(screen.getByRole("tabpanel", { name: "当前上下文" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "新建资源标签" }));
    const menu = screen.getByRole("menu", { name: "新建资源标签" });
    const workspace = within(menu).getByRole("menuitem", { name: /项目/ });
    expect(within(menu).queryByRole("menuitem", { name: /浏览器/ })).not.toBeInTheDocument();
    const terminal = within(menu).getByRole("menuitem", { name: /终端/ });
    expect(workspace).toBeDisabled();
    expect(terminal).toBeDisabled();
    expect(workspace).toHaveTextContent("项目");
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
    const workspace = screen.getByRole("tab", { name: "项目" });
    expect(workspace).toHaveFocus();
    expect(workspace).toHaveAttribute("aria-selected", "false");

    fireEvent.keyDown(workspace, { key: "Enter" });
    expect(workspace).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(workspace, { key: "Delete" });
    expect(screen.queryByRole("tab", { name: "项目" })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "当前上下文" })).toHaveAttribute("aria-selected", "true");
  });

  it("enables the workspace entry for a materialized session", () => {
    const store = new RootStore();
    store.navigation.selectSession("session-1");
    renderWorkspace(store);

    fireEvent.click(screen.getByRole("button", { name: "新建资源标签" }));
    const workspace = within(screen.getByRole("menu", { name: "新建资源标签" }))
      .getByRole("menuitem", { name: "项目" });
    expect(workspace).toBeEnabled();
    fireEvent.click(workspace);

    expect(screen.getByRole("tab", { name: "项目" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("会话私有目录")).toBeVisible();
  });

  it("keeps tab panels mounted while switching tabs", () => {
    const store = new RootStore();
    store.navigation.selectSession("session-1");
    store.resource_workspace.openWorkspace("session:session-1");
    const view = renderWorkspace(store);
    const tree = view.container.querySelector('[aria-label="项目目录"]');
    expect(tree).not.toBeNull();

    fireEvent.click(screen.getByRole("tab", { name: "当前上下文" }));
    expect(tree?.closest('[role="tabpanel"]')).toHaveAttribute("hidden");

    fireEvent.click(screen.getByRole("tab", { name: "项目" }));
    expect(view.container.querySelector('[aria-label="项目目录"]')).toBe(tree);
  });
});

function renderWorkspace(store: RootStore) {
  runInAction(() => { store.reading_connection.state = "connected"; });
  return render(
    <AppearanceProvider store={new AppearanceStore()}><RootStoreProvider store={store}>
      <ResourceWorkspace />
      <div id="overlay-root" />
    </RootStoreProvider></AppearanceProvider>,
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


it.each([false, true])("right add menu opens the body device with control permission (native=%s)", async (native) => {
  const root = new RootStore();
  const device = "11111111-1111-4111-8111-111111111111";
  const context = root.network.context(device);
  runInAction(() => {
    root.network.devices = [{ id: device, name: "远程 Mac", platform: "macos", enabled: true, online: true, desktop_view: true, desktop_control: true }];
  });
  root.navigation.selectSession("one", true, device);
  vi.spyOn(root.local_files, "native_host", "get").mockReturnValue(true);
  vi.spyOn(nativeMenu, "supportsNativeResourceMenu").mockReturnValue(native);
  const open = vi.spyOn(context.resources, "openDesktop").mockImplementation(() => {});
  const popup = vi.fn();
  vi.spyOn(nativeMenu, "createResourceMenu").mockImplementation(async (_workspace, _browser, _terminal, desktop) => ({
    popup: async (...args) => { popup(...args); desktop?.(); }, dispose: async () => {},
  }));
  const view = renderWorkspace(root);
  fireEvent.click(screen.getByRole("button", { name: "新建资源标签" }));
  if (!native) fireEvent.click(screen.getByRole("menuitem", { name: "远程桌面" }));
  await vi.waitFor(() => expect(open).toHaveBeenCalledWith(device, "远程 Mac", true));
  if (native) expect(popup.mock.calls[0]?.[3]).toBe(true);
  view.unmount(); root.dispose();
});

it("device selector only selects devices and does not open desktops", () => {
  const root = new RootStore();
  vi.spyOn(root.network, "refresh").mockResolvedValue();
  const device = "11111111-1111-4111-8111-111111111111";
  runInAction(() => { root.network.devices = [{ id: device, name: "远程 Mac", platform: "macos", enabled: true, online: true, desktop_view: true, desktop_control: true }]; });
  const view = render(<AppearanceProvider store={new AppearanceStore()}><RootStoreProvider store={root}>
    <DevicePicker device={device} aria_label="切换设备" on_select={() => {}} />
  </RootStoreProvider></AppearanceProvider>);
  fireEvent.click(screen.getByRole("button", { name: "切换设备" }));
  expect(screen.queryByText(/查看.*桌面/)).not.toBeInTheDocument();
  view.unmount(); root.dispose();
});
