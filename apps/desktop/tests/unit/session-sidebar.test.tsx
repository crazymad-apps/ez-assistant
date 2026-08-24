import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApplicationSnapshot,
  SessionSummary,
} from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { SessionSidebar } from "../../src/features/sessions/SessionSidebar";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("SessionSidebar grouping", () => {
  it("filters the current session list by title without a search scope", () => {
    const store = connectedStore();
    const history_search = vi.spyOn(store, "searchConversationHistory");

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "搜索会话" }));
    const search_input = screen.getByRole("searchbox", { name: "搜索会话名称" });

    expect(screen.queryByRole("button", { name: "选择历史检索范围" })).not.toBeInTheDocument();
    fireEvent.change(search_input, { target: { value: "工作区" } });
    expect(screen.getByRole("button", { name: /^工作区会话/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^未绑定会话/ })).not.toBeInTheDocument();
    expect(screen.getByText("1 个结果")).toBeInTheDocument();
    expect(history_search).not.toHaveBeenCalled();

    fireEvent.change(search_input, { target: { value: "消息正文里的词" } });
    expect(screen.getByText("没有匹配的会话名称")).toBeInTheDocument();
  });

  it("lets the global new-session entry choose a workspace or an independent session", () => {
    const store = connectedStore();
    const create_session = vi.spyOn(store, "createSession").mockResolvedValue();

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "新对话" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "project" }));
    expect(create_session).toHaveBeenCalledWith("workspace-1");

    fireEvent.click(screen.getByRole("button", { name: "新对话" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "独立会话" }));
    expect(create_session).toHaveBeenCalledWith(null);
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
    expect(screen.getByRole("button", { name: /^工作区会话/ }).parentElement).toHaveAttribute(
      "data-indent",
      "nested",
    );
    expect(screen.getByRole("button", { name: /^未绑定会话/ }).parentElement).toHaveAttribute(
      "data-indent",
      "root",
    );

    fireEvent.click(screen.getByRole("button", { name: "工作空间" }));

    expect(screen.queryByRole("button", { name: /^工作区会话/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "独立会话" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^未绑定会话/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "独立会话" }));

    expect(screen.queryByRole("button", { name: /^未绑定会话/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "展开独立会话" })).toBeInTheDocument();
  });

  it("creates an unbound session from the top-level unbound section", () => {
    const store = connectedStore();
    const create_session = vi.spyOn(store, "createSession").mockResolvedValue();

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
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);

    render(
      <RootStoreProvider store={store}>
        <SessionSidebar />
      </RootStoreProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "project 工作空间操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移除工作空间…" }));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("不会删除本地目录或历史会话"));
    expect(confirm).toHaveBeenCalledWith(expect.stringContaining("重新添加此目录后恢复显示"));
    expect(remove_workspace).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "project 工作空间操作" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "移除工作空间…" }));
    expect(remove_workspace).toHaveBeenCalledWith("workspace-1");

    confirm.mockRestore();
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
    fireEvent.click(screen.getByRole("button", { name: "新对话" }));
    expect(screen.queryByRole("menuitem", { name: "project" })).not.toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "独立会话" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "搜索会话" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "搜索会话名称" }), {
      target: { value: "工作区" },
    });
    expect(screen.getByText("没有匹配的会话名称")).toBeInTheDocument();
  });

  it("shows a loading indicator instead of a timestamp for a running session", () => {
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
    expect(session.querySelector("[aria-label='正在运行']")).toBeInTheDocument();
    expect(session.querySelector("time")).not.toBeInTheDocument();
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
    protocol_version: 1,
    runtime_version: "test",
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
    workspaces: [{
      workspace_id: "workspace-1",
      user_directory: "/workspace/project",
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
    capabilities: {
      conversation_paging: true,
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
    model_key: "fixture",
    lifecycle: "active",
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
