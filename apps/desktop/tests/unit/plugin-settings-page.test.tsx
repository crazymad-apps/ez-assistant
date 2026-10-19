import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { InstalledPluginSnapshot, PluginMarketEntry, PluginOperationSnapshot } from "@ez-assistant/protocol";
import { AppMessageProvider } from "../../src/components/AppMessage";
import { PluginSettingsPage } from "../../src/features/settings/SettingsDialog/PluginSettingsPage";
import type { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const plugin = (sha256: string | null): InstalledPluginSnapshot => ({
  name: "warehouse-cad-guidance",
  display_name: "库房 CAD 绘图资料",
  description: "密集架库房布置规则",
  origin: { type: "center", center_id: "center-test" },
  version: sha256 ? "0.1.0" : null,
  sha256,
  enabled: Boolean(sha256),
  package_state: "available",
  settings_revision: "revision-1",
  configuration: [],
  external_software: [],
  skills: [],
  mcp: [],
});

const market: PluginMarketEntry = {
  name: "warehouse-cad-guidance",
  display_name: "库房 CAD 绘图资料",
  description: "密集架库房布置规则",
  versions: [{ version: "0.1.0", sha256: "a".repeat(64), size_bytes: 1024, archive_format: "zip", skills: [], mcp: [] }],
};

function renderPage(installed: InstalledPluginSnapshot | null, cleanup_blocked = false, operations: readonly PluginOperationSnapshot[] = []) {
  const command = vi.fn(async (request: { type: string; payload?: { expected_active_sha256?: string | null } }) => {
    if (request.type === "list_plugins") return { payload: { plugins: installed ? [installed] : [], operations, cleanup_blocked } };
    if (request.type === "list_plugin_market") return { payload: { plugins: [market] } };
    if (request.type === "mutate_plugin_settings") return { payload: { plugin: { ...installed, enabled: false, settings_revision: "revision-2" } } };
    if (request.type === "resolve_plugin_package") return { payload: {
      name: market.name, version: "0.1.0", sha256: "a".repeat(64), size_bytes: 1024,
      format: "zip", origin: { type: "center", center_id: "center-test" }, expected_active_sha256: request.payload?.expected_active_sha256 ?? null,
    } };
    if (request.type === "begin_plugin_install") return { payload: { key: "plugin-auto-update", kind: "update", phase: "accepted", committed_names: [], cancel_requested: false, error_code: null, cleanup_required: false, received_bytes: null, total_bytes: null } };
    if (request.type === "get_plugin_operation") return { payload: { key: "plugin-auto-update", kind: "update", phase: "succeeded", committed_names: [market.name], cancel_requested: false, error_code: null, cleanup_required: false, received_bytes: null, total_bytes: null } };
    throw new Error(`Unexpected command: ${request.type}`);
  });
  const store = {
    local_runtime_client: { command, listPluginConfigFiles: vi.fn(async () => ({ files: [] })) },
    local_connection: { capabilities: { mode: "enterprise" } },
  } as unknown as RootStore;
  render(<AppMessageProvider><RootStoreProvider store={store}><PluginSettingsPage onDirtyChange={vi.fn()} /></RootStoreProvider></AppMessageProvider>);
  return command;
}

it("shows one catalog entry and keeps the card detail separate from its actions menu", async () => {
  renderPage(plugin("a".repeat(64)));
  const action = await screen.findByRole("button", { name: "操作 库房 CAD 绘图资料" });
  expect(screen.getAllByText("库房 CAD 绘图资料")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "安装 库房 CAD 绘图资料" })).not.toBeInTheDocument();
  fireEvent.click(action);
  expect(screen.getByRole("menu", { name: "库房 CAD 绘图资料 操作" })).toBeVisible();
  expect(screen.getByRole("menuitem", { name: "停用插件" })).toBeVisible();
  expect(screen.getByRole("menuitem", { name: "检查更新" })).toBeVisible();
  expect(screen.getByRole("menuitem", { name: "卸载插件…" })).toBeVisible();
  expect(screen.queryByText("包内能力")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "查看 库房 CAD 绘图资料 详情" }));
  expect(screen.getByText("包内能力")).toBeVisible();
});

it("searches the unified catalog and offers installation for a market package", async () => {
  const command = renderPage(null);
  await screen.findByRole("button", { name: "安装 库房 CAD 绘图资料" });
  fireEvent.change(screen.getByRole("searchbox", { name: "搜索插件" }), { target: { value: "不存在" } });
  expect(screen.getByText("没有匹配的插件。")).toBeVisible();
  fireEvent.change(screen.getByRole("searchbox", { name: "搜索插件" }), { target: { value: "CAD" } });
  fireEvent.click(screen.getByRole("button", { name: "安装 库房 CAD 绘图资料" }));
  await waitFor(() => expect(command).toHaveBeenCalledWith({ type: "resolve_plugin_package", payload: { name: market.name, expected_active_sha256: null } }));
  expect(screen.getByRole("dialog", { name: "安装插件" })).toBeVisible();
});

it("keeps retained plugin settings reachable while its install action is a plus button", async () => {
  renderPage(plugin(null));
  await screen.findByRole("button", { name: "安装 库房 CAD 绘图资料" });
  fireEvent.click(screen.getByRole("button", { name: "查看 库房 CAD 绘图资料 详情" }));
  expect(screen.getByText(/当前未安装插件包；原有配置与运行数据保留/)).toBeVisible();
});

it("shows a saved plugin setting in the app message layer instead of an inline banner", async () => {
  renderPage(plugin("a".repeat(64)));
  fireEvent.click(await screen.findByRole("button", { name: "操作 库房 CAD 绘图资料" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "停用插件" }));
  const message = await screen.findByText("插件设置已保存。");
  expect(message.closest('[role="status"]')).toBeInTheDocument();
  expect(screen.queryByText("插件设置已保存。", { selector: "p" })).not.toBeInTheDocument();
});

it("views a completed operation record without announcing completion again", async () => {
  const completed: PluginOperationSnapshot = {
    key: "plugin-completed", kind: "install", phase: "succeeded",
    committed_names: [market.name], cancel_requested: false, error_code: null,
    cleanup_required: false, received_bytes: null, total_bytes: null,
  };
  renderPage(plugin("a".repeat(64)), false, [completed]);
  fireEvent.click(await screen.findByRole("button", { name: "查看" }));
  expect(screen.getByRole("dialog", { name: "安装插件" })).toBeVisible();
  expect(screen.getByText("插件操作已完成")).toBeVisible();
  expect(screen.queryByText("插件操作已完成。")).not.toBeInTheDocument();
});

it("reports the current market package as up to date without starting another installation", async () => {
  const command = renderPage(plugin("a".repeat(64)));
  fireEvent.click(await screen.findByRole("button", { name: "操作 库房 CAD 绘图资料" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "检查更新" }));
  expect(await screen.findByText("插件已是最新版本。")).toBeVisible();
  expect(command).not.toHaveBeenCalledWith(expect.objectContaining({ type: "begin_plugin_install" }));
});

it("keeps plugin actions available while a cleanup warning is recorded", async () => {
  renderPage(plugin("a".repeat(64)), true);
  expect(await screen.findByText("插件目录存在待清理记录，应用会自动重试，不影响其他插件操作。")).toBeVisible();
  expect(screen.getByRole("button", { name: "上传插件" })).toBeEnabled();
  fireEvent.click(screen.getByRole("button", { name: "操作 库房 CAD 绘图资料" }));
  expect(screen.getByRole("menuitem", { name: "检查更新" })).toBeEnabled();
  expect(screen.getByRole("menuitem", { name: "卸载插件…" })).toBeEnabled();
});

it("automatically starts a center update when the catalog shows a newer package", async () => {
  const command = renderPage({ ...plugin("b".repeat(64)), version: "0.0.9" });
  await waitFor(() => expect(command).toHaveBeenCalledWith({ type: "resolve_plugin_package", payload: { name: market.name, expected_active_sha256: "b".repeat(64) } }));
  await waitFor(() => expect(command).toHaveBeenCalledWith(expect.objectContaining({ type: "begin_plugin_install" })));
  expect(screen.queryByRole("button", { name: "确认并开始更新" })).not.toBeInTheDocument();
});
