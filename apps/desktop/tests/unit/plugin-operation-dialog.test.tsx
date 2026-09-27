import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { PluginOperationSnapshot } from "@ez-assistant/protocol";
import { PluginOperationDialog } from "../../src/features/settings/SettingsDialog/PluginOperationDialog";

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("starts installation over HTTP when randomUUID is unavailable", async () => {
  vi.stubGlobal("crypto", {});
  const accepted: PluginOperationSnapshot = { key: "plugin-test", kind: "install", phase: "accepted",
    committed_names: [], cancel_requested: false, error_code: null, cleanup_required: false,
    received_bytes: null, total_bytes: null };
  const command = vi.fn(async () => ({ payload: accepted }));
  render(<PluginOperationDialog client={{ command } as never} intent={{ type: "install", packages: [{
    name: "sample", version: "1.0", sha256: "a".repeat(64), size_bytes: 1024,
    format: "zip", origin: { type: "center", center_id: "enterprise-1" }, expected_active_sha256: null,
  }] }} on_close={vi.fn()} on_complete={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "确认并开始安装" }));
  await waitFor(() => expect(command).toHaveBeenCalledWith(expect.objectContaining({
    type: "begin_plugin_install", payload: expect.objectContaining({ key: expect.stringMatching(/^plugin-\d+-[0-9a-f]+$/) }),
  })));
  expect(screen.queryByText("crypto.randomUUID is not a function")).not.toBeInTheDocument();
});

it("keeps installation confirmation open while Host runs and closes only after cancellation settles", async () => {
  const accepted: PluginOperationSnapshot = { key: "plugin-test", kind: "install", phase: "accepted",
    committed_names: [], cancel_requested: false, error_code: null, cleanup_required: false,
    received_bytes: null, total_bytes: null };
  const cancelled: PluginOperationSnapshot = { ...accepted, phase: "cancelled", cancel_requested: true };
  const command = vi.fn(async (request: { type: string }) => ({ payload:
    request.type === "cancel_plugin_operation" ? cancelled : accepted }));
  const close = vi.fn();
  render(<PluginOperationDialog client={{ command } as never} intent={{ type: "install", packages: [{
    name: "sample", version: "1.0", sha256: "a".repeat(64), size_bytes: 1024,
    format: "zip", origin: { type: "center", center_id: "enterprise-1" }, expected_active_sha256: null,
  }] }} on_close={close} on_complete={vi.fn()} />);
  expect(screen.getAllByText("sample")).toHaveLength(2);
  fireEvent.click(screen.getByRole("button", { name: "确认并开始安装" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "取消操作" })).toBeVisible());
  fireEvent.keyDown(document, { key: "Escape" });
  fireEvent.mouseDown(screen.getByRole("dialog", { name: "安装插件" }).parentElement!);
  expect(close).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "取消操作" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "关闭" })).toBeVisible());
  fireEvent.click(screen.getByRole("button", { name: "关闭" }));
  expect(close).toHaveBeenCalledOnce();
});

it("shows pending cleanup as a log and allows closing the operation", () => {
  const blocked: PluginOperationSnapshot = { key: "plugin-cleanup", kind: "install", phase: "cleaning",
    committed_names: ["sample"], cancel_requested: false, error_code: "cleanup_failed", cleanup_required: true,
    received_bytes: null, total_bytes: null };
  const close = vi.fn();
  render(<PluginOperationDialog client={{ command: vi.fn() } as never} intent={null}
    initial_snapshot={blocked} on_close={close} on_complete={vi.fn()} />);
  expect(screen.getByText("插件目录清理受阻")).toBeVisible();
  expect(screen.getByText("应用会自动重试清理，其他插件操作可继续。")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "关闭" }));
  expect(close).toHaveBeenCalledOnce();
});

it("starts a discovered update without asking for another confirmation", async () => {
  const accepted: PluginOperationSnapshot = { key: "plugin-update", kind: "update", phase: "accepted",
    committed_names: [], cancel_requested: false, error_code: null, cleanup_required: false,
    received_bytes: null, total_bytes: null };
  const command = vi.fn(async () => ({ payload: accepted }));
  render(<PluginOperationDialog client={{ command } as never} intent={{ type: "install", packages: [{
    name: "sample", version: "2.0", sha256: "b".repeat(64), size_bytes: 1024,
    format: "zip", origin: { type: "center", center_id: "enterprise-1" }, expected_active_sha256: "a".repeat(64),
  }] }} auto_begin on_close={vi.fn()} on_complete={vi.fn()} />);
  expect(screen.getByRole("dialog", { name: "更新插件" })).toBeVisible();
  expect(screen.queryByRole("button", { name: "确认并开始安装" })).not.toBeInTheDocument();
  await waitFor(() => expect(command).toHaveBeenCalledWith(expect.objectContaining({ type: "begin_plugin_install" })));
});

it("explains that a full transfer bar still leaves package preparation running", () => {
  const preparing: PluginOperationSnapshot = { key: "plugin-preparing", kind: "update", phase: "preparing",
    committed_names: [], cancel_requested: false, error_code: null, cleanup_required: false,
    received_bytes: 62_036_210, total_bytes: 62_036_210 };
  render(<PluginOperationDialog client={{ command: vi.fn(async () => ({ payload: preparing })) } as never}
    intent={null} initial_snapshot={preparing} on_close={vi.fn()} on_complete={vi.fn()} />);
  expect(screen.getByText("文件已接收，正在校验并解包")).toBeVisible();
  expect(screen.getByText("文件传输：59.2 MiB / 59.2 MiB · 100%")).toBeVisible();
  expect(screen.getByRole("button", { name: "取消操作" })).toBeVisible();
});
