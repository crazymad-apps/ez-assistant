import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApplicationSnapshot, SessionSummary } from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { SessionSidebar } from "../../src/features/sessions/SessionSidebar";

afterEach(cleanup);

describe("SessionSidebar grouping", () => {
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
      state: "ready",
      schema_version: 1,
      default_model: "fixture",
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
