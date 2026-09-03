import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApplicationSnapshot, SessionSummary } from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { AppShell } from "../../src/app/AppShell";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("AppShell session header", () => {
  it("shows the milestone version next to the application title", () => {
    renderShell(storeWithSessions());

    expect(screen.getByText(`v${__APP_VERSION__}`)).toBeVisible();
  });

  it("opens settings from the sidebar and restores focus after closing", async () => {
    const store = storeWithSessions();
    renderShell(store);

    const opener = screen.getByRole("button", { name: "设置" });
    opener.focus();
    fireEvent.click(opener);
    await screen.findByRole("dialog", { name: "设置" });
    fireEvent.click(screen.getByRole("button", { name: "关闭设置" }));

    await waitFor(() => expect(opener).toHaveFocus());
  });

  it("renames inline without restoring the removed idle status tag", () => {
    const store = storeWithSessions();
    const rename = vi.spyOn(store, "renameSession").mockResolvedValue(true);
    renderShell(store);

    expect(screen.getByLabelText("会话标题栏")).not.toHaveTextContent("空闲");
    fireEvent.click(screen.getByRole("button", { name: "First session" }));
    const input = screen.getByRole("textbox", { name: "会话标题" });
    fireEvent.change(input, { target: { value: "Renamed session" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(rename).toHaveBeenCalledWith("session-1", "Renamed session");
  });

  it("uses only the switch label to identify a proxied session", () => {
    const store = new RootStore();
    const snapshot = applicationSnapshot();
    snapshot.active_sessions[0] = {
      ...snapshot.active_sessions[0],
      proxy: {
        controller_session_id: "controller-session",
        changed_at_ms: 2,
      },
    };
    snapshot.controller_availability = {
      status: "available",
      session_id: "controller-session",
    };
    store.projection.applyApplicationSnapshot({ observed_sequence: 1, value: snapshot });
    store.navigation.selectSession("session-1");
    renderShell(store);

    expect(screen.getAllByText("主控代理")).toHaveLength(1);
    expect(screen.getByRole("switch", { name: "主控代理" })).toBeChecked();
  });

  it("does not duplicate output hosting state in the session header", () => {
    const store = new RootStore();
    const snapshot = applicationSnapshot();
    snapshot.active_sessions[0] = {
      ...snapshot.active_sessions[0]!,
      role: "controller",
      pc_output_hosting: { device_id: "device-1", device_name: "客厅终端" },
    };
    snapshot.controller_availability = { status: "available", session_id: "session-1" };
    store.projection.applyApplicationSnapshot({ observed_sequence: 1, value: snapshot });
    store.navigation.selectSession("session-1");
    renderShell(store);

    expect(within(screen.getByLabelText("会话标题栏")).queryByRole("button", {
      name: "PC 输出已托管至 客厅终端",
    })).not.toBeInTheDocument();
  });

  it("does not finish title editing when Enter only confirms input method composition", () => {
    const store = storeWithSessions();
    const rename = vi.spyOn(store, "renameSession").mockResolvedValue(true);
    renderShell(store);

    fireEvent.click(screen.getByRole("button", { name: "First session" }));
    const input = screen.getByRole("textbox", { name: "会话标题" });
    fireEvent.compositionStart(input);
    fireEvent.change(input, { target: { value: "New title" } });
    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Enter", keyCode: 13 });

    expect(rename).not.toHaveBeenCalled();
    expect(input).toHaveValue("New title");

    fireEvent.keyUp(input, { key: "Enter", keyCode: 13 });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 13 });
    expect(rename).toHaveBeenCalledWith("session-1", "New title");
  });

  it("switches recent sessions and keeps pin/archive in one more menu", () => {
    const store = storeWithSessions();
    const select = vi.spyOn(store, "selectSession").mockResolvedValue();
    const pin = vi.spyOn(store, "setSessionPinned").mockResolvedValue(true);
    const archive = vi.spyOn(store, "archiveSession").mockResolvedValue(true);
    renderShell(store);

    fireEvent.click(screen.getByRole("button", { name: "选择最近会话" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Second session" }));
    expect(select).toHaveBeenCalledWith("session-2");

    fireEvent.click(screen.getByRole("button", { name: "更多会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "固定会话" }));
    expect(pin).toHaveBeenCalledWith("session-1", true);

    fireEvent.click(screen.getByRole("button", { name: "更多会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "归档会话" }));
    expect(archive).toHaveBeenCalledWith("session-1");
  });

  it("separates session actions with a standalone non-focusable line", () => {
    renderShell(storeWithSessions());

    fireEvent.click(screen.getByRole("button", { name: "更多会话操作" }));
    const menu = screen.getByRole("menu", { name: "会话操作" });
    const separator = within(menu).getByRole("separator");
    const export_action = within(menu).getByRole("menuitem", { name: "导出 Markdown" });
    const clear_action = within(menu).getByRole("menuitem", { name: "清空会话历史…" });

    expect(separator).toHaveAttribute("aria-orientation", "horizontal");
    expect(separator.tabIndex).toBe(-1);
    expect(separator.previousElementSibling).toBe(export_action);
    expect(separator.nextElementSibling).toBe(clear_action);
    expect(clear_action).toBeDisabled();
    export_action.focus();
    fireEvent.keyDown(export_action, { key: "ArrowDown" });
    const next_action = within(menu).getByRole("menuitem", { name: "固定会话" });
    expect(next_action).toHaveFocus();
    fireEvent.keyDown(next_action, { key: "ArrowUp" });
    expect(export_action).toHaveFocus();
  });

  it("keeps the context toggle beside the session more button while the sidebar opens and closes", () => {
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(1400);
    const store = storeWithSessions();
    store.navigation.setViewportWidth(1400);
    renderShell(store);

    const header = screen.getByLabelText("会话标题栏");
    const more = within(header).getByRole("button", { name: "更多会话操作" });
    const collapse = within(header).getByRole("button", { name: "收起当前上下文" });
    expect(more.compareDocumentPosition(collapse) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(within(screen.getByLabelText("当前上下文")).queryByRole("button", {
      name: "收起当前上下文",
    })).not.toBeInTheDocument();

    fireEvent.click(collapse);

    expect(store.navigation.right_sidebar_open).toBe(false);
    expect(within(header).getByRole("button", { name: "展开当前上下文" })).toBeVisible();
  });

  it("resizes the visible sidebar with separator keyboard semantics and restores defaults", () => {
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(1400);
    const store = storeWithSessions();
    store.navigation.setViewportWidth(1400);
    renderShell(store);

    const separator = screen.getByRole("separator", { name: "调整会话栏宽度" });
    expect(separator).toHaveAttribute("aria-valuenow", "286");
    fireEvent.keyDown(separator, { key: "ArrowRight" });
    expect(store.navigation.left_sidebar_width).toBe(294);
    fireEvent.keyDown(separator, { key: "ArrowRight", shiftKey: true });
    expect(store.navigation.left_sidebar_width).toBe(318);
    fireEvent.keyDown(separator, { key: "Home" });
    expect(store.navigation.left_sidebar_width).toBe(220);
    fireEvent.doubleClick(separator);
    expect(store.navigation.left_sidebar_width).toBe(286);

    store.navigation.setSidebarWidth("right", 400);
    fireEvent.click(screen.getByRole("button", { name: "更多会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "恢复默认布局" }));
    expect(store.navigation.left_sidebar_open).toBe(true);
    expect(store.navigation.right_sidebar_open).toBe(true);
    expect(store.navigation.right_sidebar_width).toBe(326);
  });

  it("updates sidebar width immediately while pointer dragging", () => {
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(1400);
    const store = storeWithSessions();
    store.navigation.setViewportWidth(1400);
    renderShell(store);

    const separator = screen.getByRole("separator", { name: "调整会话栏宽度" });
    const captured = new Set<number>();
    Object.defineProperties(separator, {
      setPointerCapture: { value: (pointer_id: number) => captured.add(pointer_id) },
      hasPointerCapture: { value: (pointer_id: number) => captured.has(pointer_id) },
      releasePointerCapture: { value: (pointer_id: number) => captured.delete(pointer_id) },
    });

    fireEvent.pointerDown(separator, { button: 0, clientX: 300, pointerId: 7 });
    expect(document.documentElement).toHaveAttribute("data-sidebar-resizing", "left");
    fireEvent.pointerMove(separator, { clientX: 332, pointerId: 7 });
    expect(store.navigation.left_sidebar_width).toBe(318);
    fireEvent.pointerUp(separator, { clientX: 332, pointerId: 7 });
    expect(document.documentElement).not.toHaveAttribute("data-sidebar-resizing");
  });

  it("previews the exact runtime impact before permanently deleting a session", async () => {
    const store = storeWithSessions();
    const prepared = {
      session: applicationSnapshot().active_sessions[0]!,
      impact: {
        message_count: 12,
        run_count: 3,
        child_task_count: 2,
        attachment_count: 1,
      },
      confirmation_token: "delete-confirmation",
      expires_at_ms: Date.now() + 60_000,
    };
    const prepare = vi.spyOn(store, "prepareDeleteSession").mockResolvedValue(prepared);
    const remove = vi.spyOn(store, "deleteSession").mockResolvedValue(true);
    renderShell(store);

    fireEvent.click(screen.getByRole("button", { name: "更多会话操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "永久删除" }));
    await screen.findByRole("dialog", { name: "永久删除这个会话？" });

    expect(prepare).toHaveBeenCalledWith("session-1");
    expect(screen.getByText(/12 条消息.*3 条运行记录.*2 个子任务.*1 个附件引用/)).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith(prepared));
  });

  it("docks the selected child task directly below the main session header", () => {
    const store = storeWithSessions();
    store.live_execution.child_tasks.set("child-1", {
      child_task_id: "child-1",
      session_id: "session-1",
      parent_run_id: "run-1",
      parent_tool_call_id: "call-1",
      title: "检查恢复与一致性",
      status: "completed",
      variant: "build",
      cancel_requested: false,
      final_text: "检查完成",
      error: null,
      created_at_ms: 2,
      started_at_ms: 3,
      finished_at_ms: 4,
    });
    store.navigation.openChildTask("child-1");
    renderShell(store);

    const main_header = screen.getByLabelText("会话标题栏");
    const child_header = screen.getByRole("region", { name: "子任务标题栏" });
    expect(main_header.compareDocumentPosition(child_header) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(child_header).toHaveTextContent("检查恢复与一致性");
    expect(child_header).toHaveTextContent("已完成");
    expect(screen.queryByText("子 Agent", { exact: true })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "返回主会话" }));
    expect(store.navigation.selected_child_task_id).toBeNull();
  });
});

function renderShell(store: RootStore) {
  return render(
    <RootStoreProvider store={store}>
      <AppShell />
    </RootStoreProvider>,
  );
}

function storeWithSessions(): RootStore {
  const store = new RootStore();
  store.projection.applyApplicationSnapshot({
    observed_sequence: 1,
    value: applicationSnapshot(),
  });
  store.navigation.selectSession("session-1");
  return store;
}

function applicationSnapshot(): ApplicationSnapshot {
  return {
    runtime_lifecycle: "running",
    configuration: {
      config_path: null,
      revision: "fixture-revision",
      state: "ready",
      schema_version: 1,
      default_model: "fixture",
      auxiliary_vision_model: null,
      issues: [],
    },
    models: [],
    workspaces: [],
    active_sessions: [
      sessionSummary("session-1", "First session", 2),
      sessionSummary("session-2", "Second session", 1),
    ],
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

function sessionSummary(session_id: string, title: string, updated_at_ms: number): SessionSummary {
  return {
    session_id,
    title,
    model_key: "fixture",
    lifecycle: "active",
    role: "standard",
    current_variant: "build",
    approval_mode: "ask",
    workspace_id: null,
    active_run_id: null,
    message_count: 0,
    queued_input_count: 0,
    resume_required: false,
    created_at_ms: 1,
    updated_at_ms,
    archived_at_ms: null,
    is_pinned: false,
    title_origin: "user",
    pending_approval_count: 0,
    active_child_count: 0,
    active_run_status: null,
  };
}
