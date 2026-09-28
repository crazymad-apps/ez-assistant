import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApplicationSnapshot,
  SessionSummary,
} from "@ez-assistant/protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { SessionSidebar as ControlledSessionSidebar } from "../../src/features/sessions/SessionSidebar";
import { SessionSearchDialog } from "../../src/features/sessions/SessionSearchDialog";
import { useState } from "react";

function SessionSidebar() {
  const [search_open, setSearchOpen] = useState(false);
  return <>
    <ControlledSessionSidebar on_search={() => setSearchOpen(true)} on_toggle={() => {}} />
    <SessionSearchDialog on_close={() => setSearchOpen(false)} open={search_open} />
  </>;
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("SessionSidebar grouping", () => {
  it("opens agent and plugin management from separate sidebar shortcuts", () => {
    const store = connectedStore();
    render(<RootStoreProvider store={store}><SessionSidebar /></RootStoreProvider>);

    fireEvent.click(screen.getByRole("button", { name: "智能体管理" }));
    expect(store.settings.is_open).toBe(true);
    expect(store.settings.page).toBe("agents");

    store.settings.close();
    fireEvent.click(screen.getByRole("button", { name: "插件管理" }));
    expect(store.settings.is_open).toBe(true);
    expect(store.settings.page).toBe("plugins");
  });

  it("loads another summary page only when requested and keeps existing rows", async () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.active_sessions_next_offset = 100;
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });
    const page = vi.spyOn(store, "loadMoreSessions").mockImplementation(async (filter) => {
      store.projection.appendSessionPage(filter, [sessionSummary("older", "更早的会话", null)], null);
    });
    render(<RootStoreProvider store={store}><SessionSidebar /></RootStoreProvider>);
    expect(page).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "加载更多" }));
    expect(await screen.findByRole("button", { name: /^更早的会话/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^未绑定会话/ })).toBeInTheDocument();
    expect(page).toHaveBeenCalledWith("active");
    await waitFor(() => expect(screen.queryByRole("button", { name: "加载更多" })).not.toBeInTheDocument());
  });

  it("keeps the controller in a dedicated quick action and out of ordinary lists", () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    const controller = { ...sessionSummary("controller-1", "版本主控", null), role: "controller" as const };
    snapshot.active_sessions.unshift(controller);
    snapshot.controller_availability = { status: "available", session_id: controller.session_id };
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });

    render(<RootStoreProvider store={store}><SessionSidebar /></RootStoreProvider>);
    expect(screen.getByRole("button", { name: "助理会话" })).toBeEnabled();
    expect(screen.queryByRole("region", { name: "主控会话" })).not.toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "置顶会话" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "助理会话" }));
    expect(store.navigation.selected_session_id).toBe("controller-1");

    fireEvent.click(screen.getByRole("button", { name: "搜索会话" }));
    const dialog = screen.getByRole("dialog", { name: "搜索会话" });
    fireEvent.change(within(dialog).getByRole("searchbox", { name: "搜索会话名称" }), { target: { value: "无匹配" } });
    expect(screen.getByRole("button", { name: "助理会话" })).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "已归档" }));
    expect(screen.queryByRole("region", { name: "主控会话" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "关闭搜索" }));
    expect(store.navigation.list_mode).toBe("active");
  });

  it("shows a pinned session once and returns it to its workspace after unpinning", async () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.active_sessions[0] = { ...snapshot.active_sessions[0]!, is_pinned: true };
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });
    store.navigation.ensureWorkspaceExpanded("workspace-1");
    render(<RootStoreProvider store={store}><SessionSidebar /></RootStoreProvider>);

    expect(within(screen.getByRole("region", { name: "置顶会话" })).getByRole("button", { name: /^工作区会话/ })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /^工作区会话/ })).toHaveLength(1);

    store.projection.applyApplicationSnapshot({ observed_sequence: 3, value: {
      ...snapshot,
      active_sessions: [{ ...snapshot.active_sessions[0]!, is_pinned: false }, ...snapshot.active_sessions.slice(1)],
    } });
    await waitFor(() => expect(screen.queryByRole("region", { name: "置顶会话" })).not.toBeInTheDocument());
    expect(screen.getAllByRole("button", { name: /^工作区会话/ })).toHaveLength(1);
    expect(store.navigation.expanded_workspaces.has("workspace-1")).toBe(true);
  });

  it("collapses and expands pinned sessions from either header control", async () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.active_sessions[0] = { ...snapshot.active_sessions[0]!, is_pinned: true };
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });

    render(<RootStoreProvider store={store}><SessionSidebar /></RootStoreProvider>);
    const pinned = screen.getByRole("region", { name: "置顶会话" });
    const title = within(pinned).getByRole("button", { name: "置顶" });
    expect(within(pinned).getByRole("button", { name: /^工作区会话/ })).toBeInTheDocument();

    fireEvent.click(title);
    expect(title).toHaveAttribute("aria-expanded", "false");
    await waitFor(() => expect(within(pinned).queryByRole("button", { name: /^工作区会话/ })).not.toBeInTheDocument());

    fireEvent.click(within(pinned).getByRole("button", { name: "展开置顶" }));
    expect(title).toHaveAttribute("aria-expanded", "true");
    expect(within(pinned).getByRole("button", { name: /^工作区会话/ })).toBeInTheDocument();
  });

  it("shows archived sessions only in explicit search scope and restores active scope on close", () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.archived_sessions = [{ ...sessionSummary("old", "历史会话", null), lifecycle: "archived", archived_at_ms: 2 }];
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });
    render(<RootStoreProvider store={store}><SessionSidebar /></RootStoreProvider>);

    expect(screen.queryByRole("button", { name: /^历史会话/ })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "搜索会话" }));
    const dialog = screen.getByRole("dialog", { name: "搜索会话" });
    fireEvent.click(within(dialog).getByRole("button", { name: "已归档" }));
    expect(within(dialog).getByRole("button", { name: /^历史会话/ })).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "关闭搜索" }));
    expect(store.navigation.list_mode).toBe("active");
    expect(screen.queryByRole("button", { name: /^历史会话/ })).not.toBeInTheDocument();
  });

  it("queries session titles without loading conversation history", async () => {
    const store = connectedStore();
    const history_search = vi.spyOn(store, "searchConversationHistory");
    const list_page = vi.spyOn(store, "listSessionPage").mockImplementation(async (_filter, _offset, query) => ({
      sessions: store.projection.application!.active_sessions.filter((session) => session.title.includes(query ?? "")), has_more: false,
    }));

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "搜索会话" }));
    const dialog = screen.getByRole("dialog", { name: "搜索会话" });
    const search_input = within(dialog).getByRole("searchbox", { name: "搜索会话名称" });

    expect(screen.queryByRole("button", { name: "选择历史检索范围" })).not.toBeInTheDocument();
    fireEvent.change(search_input, { target: { value: "工作区" } });
    expect(await within(dialog).findByRole("button", { name: /^工作区会话/ })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /^未绑定会话/ })).not.toBeInTheDocument();
    expect(within(dialog).getByText("活动会话 · 1 个结果")).toBeInTheDocument();
    expect(history_search).not.toHaveBeenCalled();
    expect(list_page).toHaveBeenCalledWith("active", 0, "工作区");

    fireEvent.change(search_input, { target: { value: "消息正文里的词" } });
    expect(within(dialog).getByText("没有匹配的会话名称")).toBeInTheDocument();
  });

  it("opens an unbound draft directly while workspace menus retain scoped creation", () => {
    const store = connectedStore();
    const create_session = vi.spyOn(store, "openNewSessionDraft").mockImplementation(() => undefined);

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "project 项目操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "在此新建会话" }));
    expect(create_session).toHaveBeenCalledWith("workspace-1");

    fireEvent.click(screen.getByRole("button", { name: "新建会话" }));
    expect(create_session).toHaveBeenCalledWith(null);
  });

  it("shows a draft hint in menus without adding a Session row", () => {
    const store = connectedStore();
    store.openNewSessionDraft("workspace-1");
    store.new_session_drafts.updateText("workspace:workspace-1", "尚未发送");

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "project 项目操作" }));
    expect(screen.getByRole("menuitem", { name: /在此新建会话.*有草稿/ })).toBeVisible();
    expect(screen.queryByRole("button", { name: /尚未发送/ })).not.toBeInTheDocument();
  });

  it("keeps unbound sessions in a top-level disclosure independent from workspaces", () => {
    const store = new RootStore();
    store.projection.applyApplicationSnapshot({
      observed_sequence: 1,
      value: applicationSnapshot(),
    });
    store.navigation.ensureWorkspaceExpanded("workspace-1");

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    expect(screen.getByRole("button", { name: /^工作区会话/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^未绑定会话/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^工作区会话/ }).parentElement?.parentElement).toHaveAttribute(
      "data-indent",
      "nested",
    );
    expect(screen.getByRole("button", { name: /^未绑定会话/ }).parentElement?.parentElement).toHaveAttribute(
      "data-indent",
      "root",
    );

    fireEvent.click(screen.getByRole("button", { name: "项目" }));

    expect(screen.queryByRole("button", { name: /^工作区会话/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "独立会话" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^未绑定会话/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "独立会话" }));

    expect(screen.queryByRole("button", { name: /^未绑定会话/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "展开独立会话" })).toBeInTheDocument();
  });

  it("creates an unbound session from the top-level unbound section", () => {
    const store = connectedStore();
    const create_session = vi.spyOn(store, "openNewSessionDraft").mockImplementation(() => undefined);

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "新建独立会话" }));

    expect(create_session).toHaveBeenCalledWith(null);
  });

  it("confirms before removing a workspace without deleting its existing sessions", async () => {
    const store = connectedStore();
    const remove_workspace = vi.spyOn(store, "removeWorkspace").mockResolvedValue(true);

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "project 项目操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^移除项目…$/ }));
    expect(screen.getByRole("dialog", { name: "移除项目“project”？" })).toBeVisible();
    expect(screen.getByText("不会删除本地目录或历史会话。")).toBeVisible();
    expect(screen.getByText(/重新添加此目录后恢复显示/)).toBeVisible();
    expect(remove_workspace).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));

    fireEvent.click(screen.getByRole("button", { name: "project 项目操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^移除项目…$/ }));
    fireEvent.click(screen.getByRole("button", { name: "移除项目" }));
    await waitFor(() => expect(remove_workspace).toHaveBeenCalledWith("workspace-1"));
  });

  it("uses the Workspace label and opens the shared editor from its menu", () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.workspaces[0] = { ...snapshot.workspaces[0], label: "团队知识库" };
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    expect(screen.getByText("团队知识库")).toBeVisible();
    expect(screen.getByRole("button", { name: "恢复已移除项目" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "团队知识库 项目操作" }));
    expect(screen.queryByRole("menuitem", { name: /恢复已移除项目/ })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "编辑项目…" }));

    expect(store.workspace_editor).toEqual({ mode: "edit", workspace_id: "workspace-1" });
  });

  it("hides a removed workspace and all of its sessions until it is restored", () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.workspaces[0] = {
      ...snapshot.workspaces[0],
      lifecycle: "removed",
      removed_at_ms: 2,
    };
    store.projection.applyApplicationSnapshot({ observed_sequence: 2, value: snapshot });
    store.navigation.ensureWorkspaceExpanded("workspace-1");

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    expect(screen.queryByText("project")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^工作区会话/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "新建会话" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "恢复已移除项目" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "搜索会话" }));
    const dialog = screen.getByRole("dialog", { name: "搜索会话" });
    fireEvent.change(within(dialog).getByRole("searchbox", { name: "搜索会话名称" }), {
      target: { value: "工作区" },
    });
    expect(within(dialog).getByText("没有匹配的会话名称")).toBeInTheDocument();
  });

  it("shows the running indicator instead of a timestamp", () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.active_sessions[0] = {
      ...snapshot.active_sessions[0],
      active_run_id: "run-1",
    };
    store.projection.applyApplicationSnapshot({
      observed_sequence: 2,
      value: snapshot,
    });
    store.navigation.ensureWorkspaceExpanded("workspace-1");

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    const session = screen.getByRole("button", { name: /^工作区会话/ });
    expect(session.parentElement?.querySelector("svg[aria-label='正在运行']")).toBeInTheDocument();
    expect(session.parentElement?.querySelector("time")).not.toBeInTheDocument();
  });

  it("renders a large visible session list and reports the desktop baseline", () => {
    const store = connectedStore();
    const snapshot = applicationSnapshot();
    snapshot.workspaces = [];
    snapshot.active_sessions = Array.from({ length: 500 }, (_, index) =>
      sessionSummary(
        `performance-session-${index.toString().padStart(3, "0")}`,
        `性能会话-${index.toString().padStart(3, "0")}`,
        null,
      ),
    );
    store.projection.applyApplicationSnapshot({
      observed_sequence: 2,
      value: snapshot,
    });

    const started_at = performance.now();
    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );
    const elapsed_ms = performance.now() - started_at;

    expect(screen.getAllByRole("button", { name: /^性能会话-/ })).toHaveLength(500);
    console.info(JSON.stringify({
      baseline: "v0.15-desktop-session-list",
      visible_sessions: 500,
      render_ms: Number(elapsed_ms.toFixed(2)),
    }));
  });
});

function connectedStore(): RootStore {
  const store = new RootStore();
  store.connection.markConnected("instance-1", {
    mode: "personal", min_compatible_version: "0.25.2",
    runtime_version: "0.25.2",
    max_command_bytes: 64 * 1024,
    max_attachment_bytes: null,
    sse: true,
    streaming_upload: true,
    features: [],
  });
  store.projection.applyApplicationSnapshot({
    observed_sequence: 1,
    value: applicationSnapshot(),
  });
  return store;
}

function applicationSnapshot(): ApplicationSnapshot {
  return {
    runtime_lifecycle: "running",
    active_sessions_next_offset: null,
    archived_sessions_next_offset: null,
    configuration: {
      config_path: null,
      revision: "fixture-revision",
      state: "ready",
      schema_version: 1,
      issues: [],
    },
    providers: [], model_settings: { default_model: null, vision_model: null },
    workspaces: [{
      workspace_id: "workspace-1",
      label: "project",
      user_directory: "/workspace/project",
      additional_directories: [],
      agent_directory: "/runtime/workspaces/1",
      lifecycle: "active",
      created_at_ms: 1,
      updated_at_ms: 1,
      removed_at_ms: null,
    }],
    active_sessions: [
      sessionSummary("bound-session", "工作区会话", "workspace-1"),
      sessionSummary("unbound-session", "未绑定会话", null),
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

function sessionSummary(
  session_id: string,
  title: string,
  workspace_id: string | null,
): SessionSummary {
  return {
    session_id,
    title,
    model_selection: { provider_instance_id: "provider-1", model_id: "fixture" },
    lifecycle: "active",
    role: "standard",
    current_variant: "build",
    approval_mode: "ask",
    workspace_id,
    active_run_id: null,
    message_count: 0,
    queued_input_count: 0,
    resume_required: false,
    created_at_ms: 1,
    updated_at_ms: 1,
    archived_at_ms: null,
    is_pinned: false,
    title_origin: "user",
    pending_approval_count: 0,
    active_child_count: 0,
    active_run_status: null,
  };
}
