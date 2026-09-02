import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { ContextPanel } from "../../src/features/context-panel/ContextPanel";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ContextPanel", () => {
  it("stacks wide context sections in two independent vertical columns", () => {
    vi.stubGlobal("ResizeObserver", class ResizeObserverMock {
      readonly #callback: ResizeObserverCallback;

      constructor(callback: ResizeObserverCallback) {
        this.#callback = callback;
      }

      observe(target: Element) {
        this.#callback([{ contentRect: { width: 700 }, target } as ResizeObserverEntry], this as unknown as ResizeObserver);
      }

      disconnect() {}
      unobserve() {}
    });
    const store = contextStore();
    const { container } = renderPanel(store);

    const columns = container.querySelectorAll<HTMLElement>("[data-context-column]");
    expect(columns).toHaveLength(2);
    expect(within(columns[0]!).getByRole("button", { name: "会话" })).toBeVisible();
    expect(within(columns[0]!).getByRole("button", { name: "技能" })).toBeVisible();
    expect(within(columns[0]!).getByRole("button", { name: "运行记录 · 1" })).toBeVisible();
    expect(within(columns[1]!).getByRole("button", { name: "工作区" })).toBeVisible();
    expect(within(columns[1]!).getByRole("button", { name: "会话附件 · 1" })).toBeVisible();
  });

  it("collapses each section and locates a selected run through the product query", async () => {
    const store = contextStore();
    const locate = vi.spyOn(store, "locateConversationRun").mockResolvedValue(true);
    renderPanel(store);

    const session_section = screen.getByRole("button", { name: "会话" });
    expect(session_section).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Fixture Model")).toBeVisible();
    expect(screen.getByText("图片理解").nextElementSibling).toHaveTextContent("辅助视觉模型");
    fireEvent.click(session_section);
    expect(session_section).toHaveAttribute("aria-expanded", "false");
    const exiting_session = screen.getByText("Fixture Model").closest<HTMLElement>('[aria-hidden="true"]');
    expect(exiting_session).not.toBeNull();
    if (exiting_session) fireEvent.transitionEnd(exiting_session);
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

  it("shows the current Workspace label with frozen directories and opens the shared editor", () => {
    const store = contextStore();
    const open = vi.spyOn(store, "openSessionWorkspaceDirectory").mockResolvedValue();
    const copy = vi.spyOn(store, "copyWorkspacePath").mockResolvedValue();
    renderPanel(store);

    expect(screen.getByText("当前项目")).toBeVisible();
    const workspace_heading = screen.getByRole("button", { name: "工作区" });
    const workspace_edit = screen.getByRole("button", { name: "编辑工作空间" });
    const workspace_toggle = screen.getByRole("button", { name: "收起工作区" });
    expect(workspace_heading.compareDocumentPosition(workspace_edit) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(workspace_edit.compareDocumentPosition(workspace_toggle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByText("/workspace/old-project")).toBeVisible();
    expect(screen.getByText("/workspace/old-docs")).toBeVisible();
    expect(screen.getByLabelText("主目录")).toHaveAttribute("data-primary", "true");
    expect(screen.queryByText("主要")).not.toBeInTheDocument();
    expect(screen.getByText("本会话继续使用创建时的工作目录。")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "打开 /workspace/old-docs" }));
    fireEvent.click(screen.getByRole("button", { name: "复制 /workspace/old-project" }));
    fireEvent.click(screen.getByRole("button", { name: "编辑工作空间" }));

    expect(open).toHaveBeenCalledWith("session-1", 1);
    expect(copy).toHaveBeenCalledWith("/workspace/old-project");
    expect(store.workspace_editor).toEqual({ mode: "edit", workspace_id: "workspace-1" });
    expect(screen.queryByText("本会话创建时使用")).not.toBeInTheDocument();
    expect(screen.queryByText("工作区不是强沙盒。", { exact: false })).not.toBeInTheDocument();
  });

  it("shows latest and token-weighted session cache hit rates", () => {
    const store = contextStore();
    const view = store.projection.session_views.get("session-1")!;
    store.projection.applySessionSnapshot({
      observed_sequence: 2,
      value: {
        ...view,
        usage: {
          accumulated: { input_tokens: 400, output_tokens: 30, total_tokens: 430, cached_input_tokens: 170 },
          previous_turn: { input_tokens: 300, output_tokens: 20, total_tokens: 320, cached_input_tokens: 150 },
          latest_cache_hit_basis_points: 5_000,
          overall_cache_hit_basis_points: 4_250,
          context: null,
        },
      },
    });
    renderPanel(store);

    expect(screen.getByText("最新命中率").nextElementSibling).toHaveTextContent("50.0%");
    expect(screen.getByText("综合命中率").nextElementSibling).toHaveTextContent("42.5%");
  });

  it("does not duplicate PC output hosting controls in the context panel", () => {
    const store = contextStore();
    const application = store.projection.application!;
    const controller = {
      ...application.active_sessions[0]!,
      role: "controller" as const,
      pc_output_hosting: { device_id: "device-1", device_name: "客厅终端" },
    };
    store.projection.refreshApplicationSnapshot({
      observed_sequence: 2,
      value: { ...application, active_sessions: [controller] },
    });
    const view = store.projection.session_views.get("session-1")!;
    store.projection.applySessionSnapshot({
      observed_sequence: 2,
      value: { ...view, session: controller },
    });
    renderPanel(store);

    expect(screen.queryByRole("button", { name: "PC 输出托管" })).not.toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "PC 输出托管目标" })).not.toBeInTheDocument();
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
    fireEvent.click(screen.getByRole("button", { name: /系统上下文/ }));

    await waitFor(() => expect(get_system_context).toHaveBeenCalledWith("session-1"));
    const dialog = screen.getByRole("dialog", { name: "系统上下文" });
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
          latest_cache_hit_basis_points: null,
          overall_cache_hit_basis_points: null,
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
      workspaces: [{
        workspace_id: "workspace-1",
        label: "当前项目",
        user_directory: "/workspace/new-project",
        additional_directories: ["/workspace/new-docs"],
      }],
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
      workspace: {
        workspace_id: "workspace-1",
        label: "当前项目",
        primary_directory: "/workspace/old-project",
        additional_directories: ["/workspace/old-docs"],
        directories_match_current: false,
      },
      composer_capabilities: {
        selected_model_key: "fixture",
        reasoning_effort_options: [],
        image_handling: "tool",
        goal_supported: true,
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
      usage: {
        accumulated: null,
        previous_turn: null,
        latest_cache_hit_basis_points: null,
        overall_cache_hit_basis_points: null,
        context: null,
      },
      child_tasks: [],
      skill_catalog: {
        status: "ready",
        skills: [{
          name: "review-skill",
          description: "检查实现",
          source: "workspace_ez_assistant",
          model_invocable: true,
          user_invocable: true,
          enabled: true,
          health: "ready",
        }],
        diagnostics: [],
      },
      active_skills: [{
        tag: { name: "review-skill" },
        trigger: "user",
        message_id: "message-skill",
        created_at_ms: 1,
      }],
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
