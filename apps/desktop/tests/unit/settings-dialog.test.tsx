import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { runInAction } from "mobx";
import * as nativeCore from "@tauri-apps/api/core";
import { SettingsDialog } from "../../src/features/settings/SettingsDialog";
import type {
  ApplicationSnapshot,
  MemoryCapabilities,
  PersonaSnapshot,
  PinnedMemoryCollectionSnapshot,
  PermissionDocumentSnapshot,
  SessionSummary,
} from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";

vi.mock("@tauri-apps/api/core", async () => ({
  ...await vi.importActual<typeof import("@tauri-apps/api/core")>("@tauri-apps/api/core"),
  isTauri: vi.fn(() => false),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.mocked(nativeCore.isTauri).mockReturnValue(false);
});

describe("SettingsDialog model management", () => {
  it("manages MCP configuration without session refresh actions", () => {
    const store = mcpSettingsStore();
    const enqueue = vi.spyOn(store, "submitSessionCommand").mockResolvedValue(true);
    renderDialog(store);
    expect(screen.getByRole("heading", { name: "MCP" })).toBeVisible();
    expect(screen.getByRole("button", { name: /^MCP$/ }).querySelector('[data-icon="plugin"]')).toBeInTheDocument();
    expect(screen.queryByText("这里配置的服务可供所有会话使用")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "加入刷新队列" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "查看队列" })).not.toBeInTheDocument();
    expect(screen.queryByText("配置已保存，刷新后才会应用到会话。")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重新读取配置" }));
    expect(enqueue).not.toHaveBeenCalled();
  });

  it("preserves redacted MCP fields and applies explicit secret changes", async () => {
    const store = mcpSettingsStore();
    const mutate = vi.spyOn(store.settings.mcp, "mutate").mockResolvedValue(true);
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    expect(screen.getByRole("textbox", { name: "显示名称" })).toHaveFocus();
    expect(screen.getByRole("textbox", { name: "启动命令" })).toHaveValue("");
    expect(screen.getByLabelText("环境变量值 1")).toHaveValue("");
    fireEvent.change(screen.getByRole("textbox", { name: "显示名称" }), { target: { value: "GitHub 新名称" } });
    fireEvent.change(screen.getByLabelText("环境变量值 1"), { target: { value: "new-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mutate).toHaveBeenCalledWith("mcp-r1", expect.objectContaining({ type: "upsert", payload: { server: expect.objectContaining({
      display_name: "GitHub 新名称", transport: { type: "stdio", payload: { command: { mode: "keep" }, args: { mode: "keep" }, cwd: { mode: "keep" }, environment: { TOKEN: { mode: "replace", value: "new-secret" } } } },
    }) } })));
    expect(screen.queryByDisplayValue("new-secret")).not.toBeInTheDocument();
  });

  it("shows long tool descriptions as non-blocking warnings in the list and connection test", () => {
    const store = mcpSettingsStore();
    const diagnostic = { server_key: "github", code: "tool_description_long" as const, field_path: "tools/batch_design/description", message: "Tool batch_design description is 14045 bytes; retained in full and available" };
    store.settings.mcp.configuration = { ...store.settings.mcp.configuration!, needs_refresh: false, diagnostics: [diagnostic], servers: store.settings.mcp.configuration!.servers.map(server => ({ ...server, needs_refresh: false, runtime_state: "connected", tool_count: 13 })) };
    store.settings.mcp.test_result = { outcome: "success", stage: "complete", elapsed_ms: 42, tool_count: 13, diagnostic };
    renderDialog(store);
    expect(screen.getByRole("status")).toHaveTextContent("警告：github：Tool batch_design");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByText(/已连接 · 13 个工具/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    expect(screen.getByText(/连接测试成功 · 42 ms · 13 个工具/)).toBeVisible();
    expect(screen.getByRole("status")).toHaveTextContent("警告：Tool batch_design");
    expect(screen.getByRole("button", { name: /^保存$/ })).toBeEnabled();
  });

  it("uses shared MCP field selectors and submits explicit replace and remove modes", async () => {
    const store = mcpSettingsStore();
    const mutate = vi.spyOn(store.settings.mcp, "mutate").mockResolvedValue(true);
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    expect(screen.queryAllByRole("combobox")).toHaveLength(0);

    const command_mode = screen.getByRole("button", { name: "启动命令修改方式" });
    expect(command_mode).toHaveTextContent("保持原值");
    expect(command_mode).toHaveAttribute("aria-haspopup", "listbox");
    fireEvent.click(command_mode);
    const command_options = screen.getByRole("listbox", { name: "启动命令修改方式" });
    expect(within(command_options).queryByRole("option", { name: "移除" })).not.toBeInTheDocument();
    fireEvent.click(within(command_options).getByRole("option", { name: "替换" }));
    expect(command_mode).toHaveFocus();
    expect(screen.getByRole("textbox", { name: "启动命令" })).toBeEnabled();
    fireEvent.change(screen.getByRole("textbox", { name: "启动命令" }), { target: { value: "/local/mcp-server" } });

    const cwd_mode = screen.getByRole("button", { name: "工作目录 cwd修改方式" });
    fireEvent.click(cwd_mode);
    fireEvent.click(within(screen.getByRole("listbox", { name: "工作目录 cwd修改方式" })).getByRole("option", { name: "移除" }));
    expect(cwd_mode).toHaveTextContent("移除");
    expect(screen.getByRole("textbox", { name: "工作目录 cwd" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mutate).toHaveBeenCalledWith("mcp-r1", expect.objectContaining({
      type: "upsert", payload: { server: expect.objectContaining({ transport: { type: "stdio", payload: {
        command: { mode: "replace", value: "/local/mcp-server" }, args: { mode: "keep" }, cwd: { mode: "remove" }, environment: { TOKEN: { mode: "keep" } },
      } } }) },
    })));
  });

  it("keeps HTTP URL redacted when its shared selector returns to keep", async () => {
    const store = mcpSettingsStore();
    store.settings.mcp.configuration = { ...store.settings.mcp.configuration!, servers: store.settings.mcp.configuration!.servers.map(server => ({
      ...server, transport: "streamable_http", target_summary: "https://mcp.example.test", environment_keys: [], header_keys: ["Authorization"],
    })) };
    const mutate = vi.spyOn(store.settings.mcp, "mutate").mockResolvedValue(true);
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    const url_mode = screen.getByRole("button", { name: "服务 URL修改方式" });
    fireEvent.click(url_mode);
    const url_options = screen.getByRole("listbox", { name: "服务 URL修改方式" });
    expect(within(url_options).queryByRole("option", { name: "移除" })).not.toBeInTheDocument();
    fireEvent.click(within(url_options).getByRole("option", { name: "替换" }));
    fireEvent.change(screen.getByRole("textbox", { name: "服务 URL" }), { target: { value: "https://mcp.example.test/new" } });
    fireEvent.click(url_mode);
    fireEvent.click(within(screen.getByRole("listbox", { name: "服务 URL修改方式" })).getByRole("option", { name: "保持原值" }));
    expect(screen.getByRole("textbox", { name: "服务 URL" })).toBeDisabled();
    expect(screen.getByRole("textbox", { name: "服务 URL" })).toHaveValue("");
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mutate).toHaveBeenCalledWith("mcp-r1", expect.objectContaining({
      type: "upsert", payload: { server: expect.objectContaining({ transport: { type: "streamable_http", payload: {
        url: { mode: "keep" }, headers: { Authorization: { mode: "keep" } },
      } } }) },
    })));
  });

  it("clears a previous connection result when credentials change", () => {
    const store = mcpSettingsStore();
    store.settings.mcp.test_result = { outcome: "success", stage: "complete", elapsed_ms: 42, tool_count: 0 };
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    expect(screen.getByText(/连接测试成功 · 42 ms · 0 个工具/)).toBeVisible();
    fireEvent.change(screen.getByLabelText("环境变量值 1"), { target: { value: "changed-secret" } });
    expect(screen.queryByText(/连接测试成功/)).not.toBeInTheDocument();
  });

  it("binds MCP deletion to the revision the user reviewed", async () => {
    const user = userEvent.setup();
    const store = mcpSettingsStore();
    const mutate = vi.spyOn(store.settings.mcp, "mutate").mockResolvedValue(true);
    renderDialog(store);
    await user.click(screen.getByRole("button", { name: /GitHub github/ }));
    expect(screen.queryByText("已有敏感字段不会回显；保存不会立即连接服务。")).not.toBeInTheDocument();
    const heading = screen.getByRole("heading", { name: /^GitHub$/ });
    const delete_button = screen.getByRole("button", { name: "删除服务…" });
    expect(delete_button.closest("header")).toBe(heading.closest("header"));
    await user.click(delete_button);
    await user.click(within(screen.getByRole("dialog", { name: "删除 MCP 服务？" })).getByRole("button", { name: "取消" }));
    await waitFor(() => expect(delete_button).toHaveFocus());
    await user.click(delete_button);
    runInAction(() => { store.settings.mcp.configuration = { ...store.settings.mcp.configuration!, revision: "mcp-r2" }; });
    fireEvent.click(screen.getByRole("button", { name: /^删除服务$/ }));
    await waitFor(() => expect(mutate).toHaveBeenCalledWith("mcp-r1", { type: "remove", payload: { server_key: "github" } }));
  });

  it("hides MCP header deletion for new servers and disables it during editing operations", async () => {
    const store = mcpSettingsStore();
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: "添加服务" }));
    expect(screen.queryByRole("button", { name: "删除服务…" })).not.toBeInTheDocument();
    expect(screen.queryByText("已有敏感字段不会回显；保存不会立即连接服务。")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "返回 MCP 列表" }));
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    const delete_button = screen.getByRole("button", { name: "删除服务…" });
    runInAction(() => { store.settings.mcp.testing = true; });
    await waitFor(() => expect(delete_button).toBeDisabled());
    runInAction(() => { store.settings.mcp.testing = false; store.settings.pending_action = "mcp:save"; });
    await waitFor(() => expect(delete_button).toBeDisabled());
    runInAction(() => { store.settings.pending_action = null; });
    await waitFor(() => expect(delete_button).toBeEnabled());
  });

  it("previews imports with same-name replacement unchecked by default", async () => {
    const store = mcpSettingsStore();
    vi.spyOn(store.settings.mcp, "previewImport").mockResolvedValue({ diagnostics: [], entries: [
      { server_key: "github", display_name: "GitHub", transport: "stdio", conflicts_with_existing: true, warnings: [] },
      { server_key: "other", display_name: "Other", transport: "streamable_http", conflicts_with_existing: false, warnings: ["extension"] },
    ] });
    const mutate = vi.spyOn(store.settings.mcp, "mutate").mockResolvedValue(true);
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: "导入配置" }));
    fireEvent.change(screen.getByRole("textbox", { name: "MCP 配置 JSON" }), { target: { value: '{"mcpServers":{}}' } });
    fireEvent.click(screen.getByRole("button", { name: "预览导入" }));
    expect(await screen.findByRole("checkbox", { name: "替换 github" })).not.toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: "导入 1 个服务" }));
    await waitFor(() => expect(mutate).toHaveBeenCalledWith("mcp-r1", { type: "import", payload: { document: '{"mcpServers":{}}', replace_server_keys: [] } }));
  });

  it("keeps a stale MCP snapshot read-only and gates unsupported Runtime versions", () => {
    const store = mcpSettingsStore();
    store.settings.mcp.stale = true;
    const { unmount } = renderDialog(store);
    expect(screen.getByRole("button", { name: "添加服务" })).toBeDisabled();
    expect(screen.getByText(/列表已过期/)).toBeVisible();
    unmount();
    store.projection.application!.capabilities.mcp_management = false;
    renderDialog(store);
    expect(screen.getByText(/当前运行时不支持 MCP 管理/)).toBeVisible();
    expect(screen.queryByRole("button", { name: "添加服务" })).not.toBeInTheDocument();
  });

  it("asks before abandoning MCP changes and cancels connection tests on leave", async () => {
    const store = mcpSettingsStore();
    const cancel = vi.spyOn(store.settings.mcp, "cancelTest");
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: /GitHub github/ }));
    fireEvent.change(screen.getByRole("textbox", { name: "显示名称" }), { target: { value: "Changed" } });
    fireEvent.click(screen.getByRole("button", { name: "返回 MCP 列表" }));
    expect(screen.getByRole("dialog", { name: "放弃未保存的修改？" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "放弃修改" }));
    await waitFor(() => expect(screen.getByRole("heading", { name: "MCP" })).toBeVisible());
    expect(cancel).toHaveBeenCalled();
  });
  it("groups Runtime pages under one sidebar entry and separates diagnostics from local controls", () => {
    const store = settingsStore();
    vi.mocked(nativeCore.isTauri).mockReturnValue(true);
    store.settings.page = "runtime";
    renderDialog(store);
    const navigation = screen.getByRole("navigation", { name: "设置页面" });
    expect(within(navigation).getByRole("button", { name: "Runtime" })).toHaveAttribute("aria-current", "page");
    expect(within(navigation).queryByRole("button", { name: "Host 访问" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "当前连接" })).toHaveTextContent("这台电脑");
    expect(screen.queryByRole("button", { name: "停止本机 Runtime" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^状态与诊断/ }));
    const diagnostic_heading = screen.getByRole("heading", { name: "诊断信息" });
    expect(diagnostic_heading.parentElement).toHaveTextContent(
      "诊断信息/private/runtime/config.toml",
    );
    expect(screen.queryByRole("button", { name: "停止本机 Runtime" })).not.toBeInTheDocument();
    expect(within(navigation).getByRole("button", { name: "Runtime" })).toHaveAttribute("aria-current", "page");
    fireEvent.click(screen.getByRole("button", { name: "返回 Runtime" }));
    fireEvent.click(screen.getByRole("button", { name: /^本机与客户端/ }));
    expect(screen.getByRole("button", { name: "停止本机 Runtime" })).toBeVisible();
    expect(screen.getByRole("button", { name: "关闭主窗口时" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "复制诊断" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "返回 Runtime" }));
    expect(screen.getByRole("region", { name: "当前连接" })).toBeVisible();
  });

  it("hides desktop process controls in the Web settings", () => {
    const store = settingsStore();
    store.settings.page = "runtime";
    renderDialog(store);
    expect(screen.queryByRole("button", { name: /^本机与客户端/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "切换" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^访问设置/ })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /^状态与诊断/ }));
    expect(screen.getByRole("button", { name: "复制诊断" })).toBeVisible();
  });

  it("labels remote Host pages and preserves dirty access settings until navigation is confirmed", async () => {
    const store = new RootStore({ target_kind: "remote" });
    vi.mocked(nativeCore.isTauri).mockReturnValue(true);
    store.settings.is_open = true;
    store.settings.page = "runtime";
    store.connection.address = "http://remote.example:9000";
    store.connection.state = "connected";
    const hostAccessCommand = vi.fn().mockResolvedValue({
      revision: "access-1", password_configured: true, listener_state: "listening", restart_required: false, error: null,
      configuration: { remote_enabled: true, scheme: "http", port: 9000, server_names: ["remote.example"], tls_certificate: null, tls_private_key: null },
    });
    vi.spyOn(store, "runtime_client", "get").mockReturnValue({ hostAccessCommand } as unknown as RuntimeClient);
    renderDialog(store);
    expect(screen.getByRole("region", { name: "当前连接" })).toHaveTextContent("http://remote.example:9000");
    fireEvent.click(screen.getByRole("button", { name: /^访问设置/ }));
    fireEvent.change(await screen.findByLabelText("新密码"), { target: { value: "pending-password" } });
    fireEvent.click(screen.getByRole("button", { name: "返回 Runtime" }));
    const confirmation = screen.getByRole("dialog", { name: "放弃未保存的修改？" });
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.getByLabelText("新密码")).toHaveValue("pending-password");
    fireEvent.click(screen.getByRole("button", { name: "返回 Runtime" }));
    fireEvent.click(screen.getByRole("button", { name: "切换页面" }));
    expect(await screen.findByRole("region", { name: "当前连接" })).toHaveTextContent("http://remote.example:9000");
    fireEvent.click(screen.getByRole("button", { name: /^本机与客户端/ }));
    expect(screen.getByText(/以下操作作用于这台电脑/)).toBeVisible();
    expect(hostAccessCommand.mock.calls.every(([command]) => command.type === "get_status")).toBe(true);
  });

  it("saves a single port and requires explicit restart before remote access", async () => {
    const store = new RootStore({ target_kind: "local" });
    vi.mocked(nativeCore.isTauri).mockReturnValue(true);
    store.settings.is_open = true;
    store.settings.page = "host_access";
    store.connection.address = "http://127.0.0.1:7240";
    store.connection.state = "connected";
    const configuration = { remote_enabled: false, scheme: "http", port: 7240, server_names: [], tls_certificate: null, tls_private_key: null };
    const status = { revision: "one-port", password_configured: true, listener_state: "closed", restart_required: false, error: null, configuration };
    const hostAccessCommand = vi.fn().mockResolvedValueOnce(status).mockResolvedValueOnce({ ...status, revision: "new-port", restart_required: true, configuration: { ...configuration, port: 7241 } });
    const restart = vi.spyOn(store.desktop_lifecycle, "request").mockImplementation(() => {});
    vi.spyOn(store, "runtime_client", "get").mockReturnValue({ address: "http://127.0.0.1:7240", hostAccessCommand } as unknown as RuntimeClient);
    renderDialog(store);
    expect(await screen.findByLabelText("端口")).toHaveValue(7240);
    expect(screen.queryByLabelText("监听地址")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("端口"), { target: { value: "0" } });
    expect(screen.getByRole("button", { name: "保存访问设置" })).toBeDisabled();
    fireEvent.change(screen.getByLabelText("端口"), { target: { value: "7241" } });
    fireEvent.click(screen.getByRole("button", { name: "保存访问设置" }));
    expect(await screen.findByRole("button", { name: "重启本机 Runtime" })).toBeEnabled();
    expect(screen.getByRole("checkbox", { name: "允许其他设备连接" })).toBeDisabled();
    expect(hostAccessCommand).toHaveBeenLastCalledWith({ type: "configure", payload: { expected_revision: "one-port", configuration: { ...configuration, port: 7241 } } });
    expect(restart).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "重启本机 Runtime" }));
    expect(restart).toHaveBeenCalledWith("restart_runtime");
  });

  it("manages Gateway access and confirms a visible device candidate without CLI state", async () => {
    const store = settingsStore();
    store.settings.page = "devices";
    store.device_gateway.stale = false;
    store.device_gateway.snapshot = {
      enabled: true,
      available: true,
      installation_id: "installation-1",
      certificate_fingerprint: "fingerprint",
      pairing_window: { expires_at_ms: 10_000 },
      pending_pairings: [{
        pairing_request_id: "pairing-1",
        display_name: "客厅新终端",
        expires_at_ms: 10_000,
        remaining_attempts: 5,
        capabilities: {
          input_text: true,
          input_pcm16_16k_mono: false,
          output_text: true,
          output_pcm16_16k_mono: false,
          playback_cancel: false,
          display_status: true,
          display_transcript: true,
        },
      }],
      devices: [],
      speech_services: { asr: "unavailable", tts: "unavailable" },
    };
    vi.spyOn(store.device_gateway, "load").mockResolvedValue();
    const access = vi.spyOn(store.device_gateway, "setAccessEnabled").mockResolvedValue(true);
    const confirm = vi.spyOn(store.device_gateway, "confirmPairing").mockResolvedValue(true);
    renderDialog(store);

    expect(screen.getByRole("heading", { name: "智能终端" })).toBeVisible();
    expect(screen.queryByText("管理局域网智能终端接入、配对和设备身份")).not.toBeInTheDocument();
    expect(screen.queryByText("已启用并可用")).not.toBeInTheDocument();
    expect(screen.queryByText("终端发现本机后会显示在这里，输入终端展示或播报的配对码。")).not.toBeInTheDocument();
    expect(screen.queryByText("本阶段只展示 Host 当前能力，不在这里编辑服务商、模型、声音或密钥。")).not.toBeInTheDocument();
    expect(screen.getByText("客厅新终端")).toBeVisible();
    expect(screen.getByPlaceholderText("配对码")).toHaveAccessibleName("客厅新终端 配对码");
    expect(screen.getAllByText("不可用")).toHaveLength(2);
    fireEvent.click(screen.getByRole("switch", { name: "智能终端接入" }));
    expect(access).toHaveBeenCalledWith(false);

    fireEvent.change(screen.getByLabelText("客厅新终端 配对码"), { target: { value: "123456" } });
    fireEvent.click(screen.getByRole("button", { name: "确认配对" }));
    expect(confirm).toHaveBeenCalledWith("pairing-1", "123456", null);
  });

  it("keeps the paired device list concise and opens connection details on a secondary page", () => {
    const store = settingsStore();
    store.settings.page = "devices";
    store.device_gateway.stale = false;
    store.device_gateway.snapshot = {
      enabled: true,
      available: true,
      installation_id: "installation-1",
      certificate_fingerprint: "fingerprint",
      pending_pairings: [],
      devices: [{
        device_id: "device-1",
        display_name: "Node 模拟终端",
        lifecycle: "paired",
        paired_at_ms: 1_725_000_000_000,
        updated_at_ms: 1_725_000_100_000,
        revoked_at_ms: null,
        connection: {
          connected_at_ms: 1_725_000_200_000,
          output_preference: "text",
          capabilities: {
            input_text: true,
            input_pcm16_16k_mono: false,
            output_text: true,
            output_pcm16_16k_mono: false,
            playback_cancel: false,
            display_status: true,
            display_transcript: true,
          },
        },
      }],
      speech_services: { asr: "unavailable", tts: "unavailable" },
    };
    vi.spyOn(store.device_gateway, "load").mockResolvedValue();
    renderDialog(store);

    expect(screen.getByRole("img", { name: "在线" })).toBeVisible();
    expect(screen.queryByText("在线", { exact: true })).not.toBeInTheDocument();
    expect(screen.queryByText("离线不会删除设备身份或当前 PC 输出托管。")).not.toBeInTheDocument();
    expect(screen.queryByText("文字输入 · 文字输出")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "重命名" })).toBeVisible();
    expect(screen.getByRole("button", { name: "解除配对" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "查看 Node 模拟终端 详情" }));
    expect(screen.getByRole("heading", { name: "Node 模拟终端" })).toBeVisible();
    expect(screen.getByText("设备 ID").nextElementSibling).toHaveTextContent("device-1");
    expect(screen.getByText("输入能力").nextElementSibling).toHaveTextContent("文字");
    expect(screen.getByText("交互能力").nextElementSibling).toHaveTextContent("状态显示、转写显示");

    fireEvent.click(screen.getByRole("button", { name: "返回设备列表" }));
    expect(screen.getByRole("heading", { name: "智能终端" })).toBeVisible();
  });

  it("shows the Chinese skill management projection and delegates the name toggle", () => {
    const store = settingsStore();
    store.settings.page = "skills";
    store.settings.skill_management = {
      available: true,
      skills: [{ name: "review", description: "检查实现", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" }],
      diagnostics: [],
    };
    const toggle = vi.spyOn(store.settings, "setSkillEnabled").mockResolvedValue(true);
    renderDialog(store);

    expect(screen.getByRole("button", { name: "Runtime" })).toBeVisible();
    expect(screen.queryByText("文件与启停变更仅对新会话生效。")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "技能工作区范围" })).toHaveTextContent("仅用户根目录");
    expect(screen.getByText("检查实现")).toBeVisible();
    fireEvent.click(screen.getByRole("checkbox", { name: "启用 review" }));
    expect(toggle).toHaveBeenCalledWith("review", false);
  });

  it("keeps skill row actions separate and locks toggles while a change is pending", async () => {
    const store = settingsStore();
    store.settings.page = "skills";
    store.settings.skill_management = {
      available: true,
      skills: [
        { name: "review", description: "检查实现", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" },
        { name: "disabled-skill", description: "", source: "user_agents", model_invocable: true, user_invocable: false, enabled: false, health: "disabled" },
      ],
      diagnostics: [],
    };
    const load_detail = vi.spyOn(store.settings, "loadSkillDetail").mockResolvedValue();
    renderDialog(store);

    const row = screen.getByRole("button", { name: /disabled-skill/ });
    expect(within(row).queryByRole("checkbox")).not.toBeInTheDocument();
    expect(within(row).getByText("暂无描述")).toBeVisible();
    expect(screen.getByRole("checkbox", { name: "启用 disabled-skill" })).not.toBeChecked();
    runInAction(() => { store.settings.pending_skill_name = "review"; });
    await waitFor(() => {
      for (const checkbox of screen.getAllByRole("checkbox")) expect(checkbox).toBeDisabled();
    });
    expect(row).toBeEnabled();
    fireEvent.click(row);
    expect(load_detail).toHaveBeenCalledWith("disabled-skill");
  });

  it("moves the summary into the description row and renders the skill body", () => {
    const store = settingsStore();
    const skill = { name: "review", description: "检查实现", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" } as const;
    store.settings.page = "skills";
    store.settings.skill_management = { available: true, skills: [skill], diagnostics: [] };
    store.settings.skill_detail = { skill, body: "# 检查步骤\n\n确认实现与测试。", diagnostics: [] };
    const load_detail = vi.spyOn(store.settings, "loadSkillDetail").mockResolvedValue();
    renderDialog(store);

    fireEvent.click(screen.getByRole("button", { name: /review/ }));

    expect(load_detail).toHaveBeenCalledWith("review");
    expect(screen.getByRole("heading", { name: "review" })).toBeVisible();
    expect(screen.getByText("描述").nextElementSibling).toHaveTextContent("检查实现");
    expect(screen.getByRole("heading", { name: "技能正文" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "检查步骤" })).toBeVisible();
    expect(screen.getByText("确认实现与测试。")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "返回技能列表" }));
    expect(screen.getByRole("heading", { name: "技能" })).toBeVisible();
  });

  it("selects auxiliary vision through provider and online model levels", async () => {
    const store = settingsStore();
    store.settings.providers = [onlineProvider()];
    vi.spyOn(store.settings, "listProviderModels").mockResolvedValue([onlineModel()]);
    const set_vision = vi.spyOn(store.settings, "setAuxiliaryVisionModel").mockResolvedValue(true);
    renderDialog(store);
    const trigger = screen.getByRole("button", { name: "默认识图模型" });
    expect(trigger).toHaveTextContent("未配置");
    fireEvent.click(trigger);
    fireEvent.click(await screen.findByRole("menuitem", { name: /测试服务商/ }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /online-model/ }));
    await waitFor(() => expect(set_vision).toHaveBeenCalledWith({ provider_instance_id: "provider-1", model_id: "online-model" }));
  });

  it("reloads online entries on reopening and does not restore them after failure", async () => {
    const store = settingsStore();
    vi.spyOn(store.settings, "listAllFixedModels").mockResolvedValue([]);
    store.settings.providers = [onlineProvider()];
    const list = vi.spyOn(store.settings, "listProviderModels").mockResolvedValueOnce([onlineModel()]).mockRejectedValueOnce(new Error("offline"));
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: "默认模型" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: /测试服务商/ }));
    expect(await screen.findByRole("menuitemradio", { name: /online-model/ })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "默认模型" }));
    fireEvent.click(screen.getByRole("button", { name: "默认模型" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: /测试服务商/ }));
    expect(screen.queryByRole("menuitemradio", { name: /online-model/ })).not.toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("offline");
    expect(list).toHaveBeenCalledTimes(2);
  });


});

describe("SettingsDialog permission management", () => {
  it.each(["tool", "server", "all"] as const)("edits and round-trips %s MCP permissions using shared controls", async (scope) => {
    const store = settingsStore();
    store.settings.page = "permissions";
    const document = permissionDocument({ type: "session", payload: { session_id: "session-1" } }, true);
    document.rules = [{ id: "existing-mcp", effect: "ask", variants: ["build", "plan"], matcher: { type: "mcp", payload: {
      server: scope === "all" ? { type: "any" } : { type: "exact", payload: { server_key: "github" } },
      tool: scope === "tool" ? { type: "exact", payload: { tool_name: "create_issue" } } : { type: "any" },
    } } }];
    store.settings.permission_documents = [document];
    const save = vi.spyOn(store.settings, "replacePermissionDocument").mockResolvedValue(false);
    renderDialog(store);
    fireEvent.click(screen.getByRole("button", { name: "编辑规则" }));
    expect(screen.getByRole("button", { name: "选择匹配类型" })).toHaveTextContent("MCP 工具");
    if (scope !== "all") expect(screen.getByRole("textbox", { name: "服务 key" })).toHaveValue("github");
    if (scope === "tool") expect(screen.getByRole("textbox", { name: "原始工具名称" })).toHaveValue("create_issue");
    fireEvent.click(screen.getByRole("button", { name: "保存规则" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(document.scope, document.revision, { schema_version: 1, rules: document.rules }));
    expect(screen.getByRole("button", { name: "选择 MCP 匹配范围" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "选择 MCP 匹配范围" }));
    fireEvent.click(screen.getByRole("option", { name: "全部 MCP 工具" }));
    expect(screen.queryByRole("textbox", { name: "服务 key" })).not.toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "原始工具名称" })).not.toBeInTheDocument();
  });

  it("uses shared selection popovers and keeps scope tabs concise", () => {
    const store = settingsStore();
    store.settings.page = "permissions";
    store.settings.permission_documents = [
      permissionDocument({ type: "session", payload: { session_id: "session-1" } }, true),
      permissionDocument({ type: "workspace", payload: { workspace_id: "workspace-1" } }, true),
      permissionDocument({ type: "global" }, false),
    ];
    renderDialog(store);

    const scope_tabs = screen.getByRole("tablist");
    expect(scope_tabs.closest("header")).not.toBeNull();
    expect(within(scope_tabs).getByRole("tab", { name: "当前会话" })).toHaveTextContent(/^当前会话$/);
    expect(within(scope_tabs).getByRole("tab", { name: "当前会话" })).toHaveAttribute("aria-selected", "true");
    expect(within(scope_tabs).getByRole("tab", { name: "工作区" })).toHaveTextContent(/^工作区$/);
    expect(within(scope_tabs).getByRole("tab", { name: "全局" })).toHaveTextContent(/^全局$/);

    fireEvent.click(screen.getByRole("button", { name: "添加规则" }));
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();

    const effect = screen.getByRole("button", { name: "选择规则效果" });
    fireEvent.click(effect);
    const listbox = screen.getByRole("listbox");
    expect(within(listbox).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "允许",
      "询问",
      "拒绝",
    ]);
    fireEvent.click(within(listbox).getByRole("option", { name: "拒绝" }));
    expect(effect).toHaveTextContent("拒绝");
  });
});

describe("SettingsDialog memory management", () => {
  it("shows a concise empty state and opens the shared inline editor", () => {
    const store = settingsStore();
    store.settings.page = "memory";
    store.memory_settings.persona = personaSnapshot();
    store.memory_settings.capabilities = memoryCapabilities;
    store.memory_settings.collection = pinnedCollection();
    renderDialog(store);

    expect(screen.getByText("尚未添加 Pinned Memory。")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));
    expect(screen.getByText("添加 Pinned Memory")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("例如：协作偏好、常用约定")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("记录需要跨会话长期保留的稳定信息")).toBeInTheDocument();
  });
});

function settingsStore(): RootStore {
  const store = new RootStore();
  vi.spyOn(store.settings, "loadModelSettings").mockResolvedValue(true);
  store.settings.is_open = true;
  store.settings.page = "models";
  store.settings.status = {
    config_path: "/private/runtime/config.toml",
    revision: "revision-1",
    state: "ready",
    schema_version: 1,
    issues: [],
  };
  return store;
}

function mcpSettingsStore(): RootStore {
  const store = settingsStore();
  store.settings.page = "mcp";
  const session = modelSession(null);
  store.projection.application = applicationWithSession(session);
  store.navigation.selectSession(session.session_id, false);
  store.settings.mcp.configuration = { revision: "mcp-r1", needs_refresh: true, diagnostics: [], servers: [{
    server_key: "github", display_name: "GitHub", description: "管理 Issue", transport: "stdio", enabled: true,
    runtime_state: "unavailable", tool_count: 0, needs_refresh: true, target_summary: "server",
    environment_keys: ["TOKEN"], header_keys: [], startup_timeout_ms: null, tool_timeout_ms: null,
  }] };
  vi.spyOn(store.settings.mcp, "load").mockResolvedValue();
  return store;
}

function applicationWithSession(session: SessionSummary): ApplicationSnapshot {
  return {
    runtime_lifecycle: "running",
    active_sessions_next_offset: null,
    archived_sessions_next_offset: null,
    configuration: {
      config_path: null,
      revision: "revision-1",
      state: "ready",
      schema_version: 1,
      issues: [],
    },
    providers: [], model_settings: { default_model: null, vision_model: null },
    workspaces: [],
    active_sessions: [session],
    archived_sessions: [],
    controller_availability: { status: "unavailable" },
    additional_controller_count: 0,
    capabilities: {
      conversation_paging: true, mcp_tools: true, mcp_management: true, session_commands: true,
      tool_detail: true,
      queue_control: true,
      approval_queue: true,
      child_task_view: true,
      conversation_search: true,
    },
  };
}

function modelSession(active_run_id: string | null): SessionSummary {
  return {
    session_id: "session-1",
    title: "模型引用会话",
    model_selection: { provider_instance_id: "provider-1", model_id: "secondary" },
    lifecycle: "active",
    role: "standard",
    current_variant: "build",
    approval_mode: "ask",
    workspace_id: null,
    active_run_id,
    message_count: 1,
    queued_input_count: 0,
    resume_required: false,
    created_at_ms: 1,
    updated_at_ms: 1,
    archived_at_ms: null,
    is_pinned: false,
    title_origin: "user",
    pending_approval_count: 0,
    active_child_count: 0,
    active_run_status: active_run_id ? "running" : null,
  };
}

function permissionDocument(
  scope: PermissionDocumentSnapshot["scope"],
  editable: boolean,
): PermissionDocumentSnapshot {
  return {
    scope,
    revision: { type: "content", payload: { value: "revision-1" } },
    status: "ready",
    schema_version: 1,
    rules: [],
    diagnostics: [],
    editable,
  };
}

const memoryCapabilities: MemoryCapabilities = {
  max_persona_bytes: 16_384,
  max_pinned_entries: 256,
  max_pinned_category_bytes: 128,
  max_pinned_content_bytes: 16_384,
  max_attributes_per_entry: 32,
  max_attribute_key_bytes: 128,
  max_attribute_string_bytes: 4_096,
};

function personaSnapshot(): PersonaSnapshot {
  return {
    enabled: true,
    content: "结论优先",
    revision: 1,
    updated_at_ms: 1,
  };
}

function pinnedCollection(): PinnedMemoryCollectionSnapshot {
  return {
    revision: 0,
    items: [],
    capabilities: memoryCapabilities,
  };
}

function renderDialog(store: RootStore) {
  return render(
    <RootStoreProvider store={store}>
      <SettingsDialog />
    </RootStoreProvider>,
  );
}

function onlineProvider(): import("../../src/generated/assistant-protocol").ProviderSummary {
  return { provider_instance_id: "provider-1", has_api_key: true, connection: {
    display_name: "测试服务商", provider_type: "openai", endpoint: "https://example.test/v1",
    protocol_preference: "chat_completions", models_path: "/v1/models", discovery_format: "openai",
  } };
}
function onlineModel(): import("../../src/generated/assistant-protocol").DiscoveredModel {
  return { configuration: null, model_id: "online-model", display_name: null, metadata: {
    context_window_tokens: { state: "unknown" }, max_input_tokens: { state: "unknown" }, max_output_tokens: { state: "unknown" },
    reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
    streaming: "unknown", tool_choice: { auto: "unknown", none: "unknown", required: "unknown", named: "unknown" },
    tool_image_projection: "unknown", image_input: "unknown", tool_calls: "unknown", reasoning: "unknown",
    reasoning_mode: "unknown", reasoning_efforts: null, default_reasoning_effort: null,
  } };
}
