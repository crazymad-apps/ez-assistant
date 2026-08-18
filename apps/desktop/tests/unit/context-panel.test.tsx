import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { ContextPanel } from "../../src/features/context-panel/ContextPanel";

afterEach(cleanup);

describe("ContextPanel", () => {
  it("collapses each section and locates a selected run through the product query", async () => {
    const store = contextStore();
    const locate = vi.spyOn(store, "locateConversationRun").mockResolvedValue(true);
    renderPanel(store);

    const session_section = screen.getByRole("button", { name: "会话" });
    expect(session_section).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Fixture Model")).toBeVisible();
    fireEvent.click(session_section);
    expect(session_section).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Fixture Model")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /运行 #1/ }));
    await waitFor(() => expect(locate).toHaveBeenCalledWith("session-1", "run-1"));
  });

  it("shows session attachments without aggregating tool file references", () => {
    const store = contextStore();
    renderPanel(store);

    expect(screen.getByRole("button", { name: "会话附件 · 1" })).toBeVisible();
    expect(screen.getByRole("button", { name: /brief\.png/ })).toBeVisible();
    expect(screen.queryByText("工具文件")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /report\.txt/ })).not.toBeInTheDocument();
  });

  it("offers the immutable Workspace actions", () => {
    const store = contextStore();
    const open = vi.spyOn(store, "openWorkspace").mockResolvedValue();
    const copy = vi.spyOn(store, "copyWorkspacePath").mockResolvedValue();
    renderPanel(store);

    fireEvent.click(screen.getByRole("button", { name: "打开目录" }));
    fireEvent.click(screen.getByRole("button", { name: "复制路径" }));

    expect(open).toHaveBeenCalledWith("workspace-1");
    expect(copy).toHaveBeenCalledWith("/workspace/project");
    expect(screen.queryByRole("button", { name: "重新选择" })).not.toBeInTheDocument();
    expect(screen.getByText("Workspace 不是强沙盒。", { exact: false })).toBeVisible();
  });

  it("previews the frozen System Context as Markdown and can reveal its source text", async () => {
    const store = contextStore();
    const get_system_context = vi.spyOn(store, "getSystemContext").mockResolvedValue({
      session_id: "session-1",
      session_created_at_ms: 1,
      parts: ["# You are EZ Assistant.\n\nUse tools.", "<workspace_instructions file=\"AGENTS.md\">\n## 默认使用简体中文。\n</workspace_instructions>"],
    });
    renderPanel(store);

    expect(screen.queryByText(/已冻结/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /System Context/ }));

    await waitFor(() => expect(get_system_context).toHaveBeenCalledWith("session-1"));
    const dialog = screen.getByRole("dialog", { name: "System Context" });
    expect(within(dialog).getByRole("heading", { name: "You are EZ Assistant." })).toBeVisible();
    expect(within(dialog).getByRole("heading", { name: "默认使用简体中文。" })).toBeVisible();
    expect(within(dialog).getByRole("button", { name: "预览" })).toHaveAttribute("aria-pressed", "true");
    expect(within(dialog).getByText("当前会话创建时冻结的系统上下文原文", { exact: false })).toBeVisible();

    fireEvent.click(within(dialog).getByRole("button", { name: "原文" }));
    expect(within(dialog).getByText("# You are EZ Assistant.", { exact: false })).toBeVisible();
    expect(within(dialog).getByText("<workspace_instructions file=\"AGENTS.md\">", { exact: false })).toBeVisible();
    expect(within(dialog).getByRole("button", { name: "原文" })).toHaveAttribute("aria-pressed", "true");
  });

  it("degrades missing historical fields and unavailable resources without inventing zero values", () => {
    const store = contextStore();
    const application = store.projection.application!;
    store.projection.refreshApplicationSnapshot({
      observed_sequence: 2,
      value: {
        ...application,
        models: [],
        active_sessions: application.active_sessions.map((session) => ({
          ...session,
          model_key: "removed-model",
        })),
      },
    });
    const view = store.projection.session_views.get("session-1")!;
    store.projection.applySessionSnapshot({
      observed_sequence: 2,
      value: {
        ...view,
        attachments: view.attachments.map((attachment) => ({ ...attachment, state: "unavailable" })),
        runs: view.runs.map((run) => ({ ...run, created_at_ms: null, finished_at_ms: null })),
        usage: {
          accumulated: { input_tokens: null, output_tokens: null, total_tokens: null, cached_input_tokens: null },
          previous_turn: { input_tokens: null, output_tokens: null, total_tokens: null, cached_input_tokens: null },
          context: null,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
    renderPanel(store);

    expect(screen.getByText("removed-model（历史配置）")).toBeVisible();
    expect(screen.getAllByText("未提供").length).toBeGreaterThan(0);
    expect(screen.getByText("时间未记录", { exact: false })).toBeVisible();
    expect(screen.getByRole("button", { name: /brief\.png/ })).toBeDisabled();
    expect(screen.queryByRole("button", { name: /report\.txt/ })).not.toBeInTheDocument();
  });
});

function renderPanel(store: RootStore) {
  return render(
    <RootStoreProvider store={store}>
      <ContextPanel />
    </RootStoreProvider>,
  );
}

function contextStore(): RootStore {
  const store = new RootStore();
  store.projection.applyApplicationSnapshot({
    observed_sequence: 1,
    value: {
      runtime_lifecycle: "running",
      configuration: { state: "ready" },
      models: [{ model_key: "fixture", display_name: "Fixture Model" }],
      workspaces: [{ workspace_id: "workspace-1", user_directory: "/workspace/project" }],
      active_sessions: [{
        session_id: "session-1",
        workspace_id: "workspace-1",
        model_key: "fixture",
        current_variant: "build",
        approval_mode: "ask",
        message_count: 2,
      }],
      archived_sessions: [],
    },
  } as unknown as Parameters<RootStore["projection"]["applyApplicationSnapshot"]>[0]);
  store.projection.applySessionSnapshot({
    observed_sequence: 1,
    value: {
      session: {
        session_id: "session-1",
        workspace_id: "workspace-1",
        model_key: "fixture",
        current_variant: "build",
        approval_mode: "ask",
        message_count: 2,
      },
      active_run: null,
      queue: { revision: 1, state: "automatic", items: [] },
      approvals: { revision: 1, items: [], resolving_approval_id: null },
      attachments: [{
        attachment_id: "attachment-1",
        session_id: "session-1",
        original_name: "brief.png",
        size_bytes: 1024,
        agent_readable_path: "",
        state: "ready",
        created_at_ms: 1,
      }],
      file_references: [{
        message_id: "message-1",
        call_id: "call-1",
        file: {
          resource_ref_id: "resource-1",
          origin: "workspace_file",
          display_name: "report.txt",
          display_path: "report.txt",
          size_bytes: null,
          media_type: "text/plain",
          state: "available",
        },
      }],
      runs: [{
        run_id: "run-1",
        session_id: "session-1",
        attempt: 1,
        created_at_ms: 1,
        finished_at_ms: 2,
        status: "completed",
        variant: "build",
        approval_mode: "ask",
        tools: [],
      }],
      usage: { accumulated: null, previous_turn: null, context: null },
      child_tasks: [],
      conversation: {
        owner: { type: "main_session", session_id: "session-1" },
        generation: 1,
        items: [],
        previous_cursor: null,
        has_more: false,
      },
    },
  } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
  store.navigation.selectSession("session-1");
  return store;
}
