import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApplicationSnapshot,
  ApprovalSnapshot,
  QueueSnapshot,
  SessionSummary,
  SessionViewSnapshot,
  ChildTaskTreeItemSnapshot,
} from "../../src/generated/assistant-protocol";
import {
  chooseAttachmentFiles,
  uploadSelectedAttachment,
} from "../../src/native-bridge/nativeResource";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { ComposerDock } from "../../src/features/composer/ComposerDock";

vi.mock("../../src/native-bridge/nativeResource", () => ({
  cancelResourceOperation: vi.fn().mockResolvedValue(undefined),
  chooseAttachmentFiles: vi.fn(),
  releaseAttachmentSelection: vi.fn().mockResolvedValue(undefined),
  uploadSelectedAttachment: vi.fn(),
}));

describe("ComposerDock", () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("submits with Enter, preserves Shift+Enter and opens shared slash pickers", async () => {
    const user = userEvent.setup();
    const store = renderComposer();
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(true);
    const set_model = vi.spyOn(store, "setSessionModel").mockResolvedValue(true);
    const set_variant = vi.spyOn(store, "setSessionVariant").mockResolvedValue(true);
    const input = screen.getByRole("textbox", { name: "输入消息" });

    expect(input).toHaveAttribute("rows", "2");
    await user.type(input, "第一行");
    await user.keyboard("{Shift>}{Enter}{/Shift}");
    await user.type(input, "第二行");
    expect(input).toHaveValue("第一行\n第二行");
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(submit).toHaveBeenCalledWith("session-1", "第一行\n第二行", "build", []));
    await waitFor(() => expect(input).toHaveValue(""));

    await user.type(input, "/model");
    fireEvent.keyDown(input, { key: "Enter" });
    await user.click(screen.getByRole("option", { name: /备用模型/ }));
    expect(set_model).toHaveBeenCalledWith("session-1", "alternate");
    expect(screen.queryByRole("option", { name: /备用模型/ })).not.toBeInTheDocument();

    await user.type(input, "/mode");
    fireEvent.keyDown(input, { key: "Enter" });
    await user.click(screen.getByRole("option", { name: /Plan 模式/ }));
    expect(set_variant).toHaveBeenCalledWith("session-1", "plan");
  });

  it("supports queue priority, removal and explicit resume without a bottom-wide action", async () => {
    const user = userEvent.setup();
    const queue: QueueSnapshot = {
      revision: 4,
      state: "automatic",
      items: [
        { input_id: "input-1", text_preview: "先检查构建", submitted_at_ms: 1, position: 1, is_prioritized: false },
        { input_id: "input-2", text_preview: "再运行测试", submitted_at_ms: 2, position: 2, is_prioritized: false },
      ],
    };
    const store = renderComposer({ queue });
    const prioritize = vi.spyOn(store, "prioritizeQueuedInput").mockResolvedValue();
    const cancel = vi.spyOn(store, "cancelQueuedInput").mockResolvedValue();

    expect(screen.getByText("待执行队列", { exact: true })).toBeVisible();
    const first_row = screen.getByText("先检查构建").closest("div");
    expect(first_row).not.toBeNull();
    await user.click(within(first_row!).getByRole("button", { name: "优先" }));
    expect(prioritize).toHaveBeenCalledWith("session-1", "input-1", 4);
    await user.click(within(first_row!).getByRole("button", { name: "移除排队输入" }));
    expect(cancel).toHaveBeenCalledWith("session-1", "input-1");
    expect(screen.queryByRole("button", { name: /执行全部/ })).not.toBeInTheDocument();

    store.projection.applySessionSnapshot(observed(sessionView({
      queue: { ...queue, revision: 5, state: "paused_by_user" },
    }), 2));
    const resume = vi.spyOn(store, "resumeQueuedInput").mockResolvedValue();
    await user.click((await screen.findAllByRole("button", { name: "恢复" }))[0]!);
    expect(resume).toHaveBeenCalledWith("session-1", "input-1", 5);
  });

  it("lets a pending approval take over the bottom workspace and remain blocking when minimized", async () => {
    const user = userEvent.setup();
    const approval = approvalSnapshot();
    const store = renderComposer({ approvals: [approval] });
    const decide = vi.spyOn(store, "decideApproval").mockResolvedValue();
    const reject_and_stop = vi.spyOn(store, "rejectApprovalAndStopRun").mockResolvedValue();

    expect(screen.getByText("允许执行 Shell 命令？", { exact: true })).toBeVisible();
    expect(screen.queryByRole("textbox", { name: "输入消息" })).not.toBeInTheDocument();
    expect(screen.getByText("npm run build && npm test", { exact: true })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "最小化授权面板" }));
    expect(screen.getByRole("textbox", { name: "输入消息" })).toBeVisible();
    expect(screen.getByRole("button", { name: /等待审批/ })).toBeVisible();
    await user.click(screen.getByRole("button", { name: /等待审批/ }));
    await user.click(screen.getByText("当前会话", { exact: true }));
    await user.click(screen.getByRole("button", { name: "允许执行" }));
    expect(decide).toHaveBeenCalledWith("session-1", "approval-1", "allow_session");

    await user.click(screen.getByRole("button", { name: "拒绝并停止本轮" }));
    expect(reject_and_stop).toHaveBeenCalledWith("session-1", "approval-1", 1);
  });

  it("uses the parent approval workspace for a child task while keeping the child view read-only", () => {
    const approval = { ...approvalSnapshot(), child_task_id: "child-1" };
    renderComposer({ approvals: [approval], child_tasks: [childTaskItem()], read_only: true });

    expect(screen.getByText("检查恢复一致性 · 由子 Agent 请求")).toBeVisible();
    expect(screen.getByText("允许执行 Shell 命令？", { exact: true })).toBeVisible();
    expect(screen.queryByRole("textbox", { name: "输入消息" })).not.toBeInTheDocument();
  });

  it("keeps native selections local until send and submits stable attachment ids", async () => {
    const user = userEvent.setup();
    const store = renderComposer();
    vi.mocked(chooseAttachmentFiles).mockResolvedValue([{
      selection_id: "selection-1",
      original_name: "notes.md",
      size_bytes: 2048,
    }]);
    vi.mocked(uploadSelectedAttachment).mockResolvedValue({
      attachment: {
        attachment_id: "attachment-1",
        session_id: "session-1",
        original_name: "notes.md",
        size_bytes: 2048,
        agent_readable_path: "/private/runtime/attachment-1",
        state: "ready",
        created_at_ms: 1,
      },
    });
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(true);

    await user.click(screen.getByRole("button", { name: "添加附件" }));
    expect(await screen.findByText("notes.md")).toBeVisible();
    await user.type(screen.getByRole("textbox", { name: "输入消息" }), "参考附件");
    await user.click(screen.getByRole("button", { name: "发送消息" }));

    await waitFor(() => expect(uploadSelectedAttachment).toHaveBeenCalledWith(
      "session-1",
      "selection-1",
      expect.any(String),
    ));
    await waitFor(() => expect(submit).toHaveBeenCalledWith(
      "session-1",
      "参考附件",
      "build",
      ["attachment-1"],
    ));
    expect(screen.queryByText("notes.md")).not.toBeInTheDocument();
  });
});

function renderComposer(overrides: Readonly<{
  queue?: QueueSnapshot;
  approvals?: readonly ApprovalSnapshot[];
  child_tasks?: readonly ChildTaskTreeItemSnapshot[];
  read_only?: boolean;
}> = {}): RootStore {
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
  store.projection.applyApplicationSnapshot(observed(applicationSnapshot()));
  store.projection.applySessionSnapshot(observed(sessionView(overrides)));
  store.navigation.selectSession("session-1");
  render(<><RootStoreProvider store={store}><ComposerDock read_only={overrides.read_only} /></RootStoreProvider><div id="overlay-root" /></>);
  return store;
}

function applicationSnapshot(): ApplicationSnapshot {
  return {
    runtime_lifecycle: "running",
    configuration: { config_path: null, revision: "fixture-revision", state: "ready", schema_version: 1, default_model: "fixture", issues: [] },
    models: [
      model("fixture", "本地模型（7B）", true),
      model("alternate", "备用模型", false),
    ],
    workspaces: [{
      workspace_id: "workspace-1",
      user_directory: "/workspace/project",
      agent_directory: "/runtime/workspaces/1",
      lifecycle: "active",
      created_at_ms: 1,
      updated_at_ms: 1,
      removed_at_ms: null,
    }],
    active_sessions: [sessionSummary()],
    archived_sessions: [],
    capabilities: { conversation_paging: true, tool_detail: true, queue_control: true, approval_queue: true, child_task_view: true, conversation_search: true },
  };
}

function model(model_key: string, display_name: string, is_default: boolean): ApplicationSnapshot["models"][number] {
  return {
    model_key,
    display_name,
    protocol: "chat_completions",
    provider: "fixture",
    endpoint: "http://127.0.0.1/v1",
    model: `${model_key}-model`,
    context_window_tokens: 128_000,
    max_output_tokens: 4_096,
    agent_max_output_tokens: 4_096,
    effective_max_output_tokens: 4_096,
    api_key_configured: true,
    origin: "configuration_file",
    editable: true,
    deletable: true,
    is_default,
    is_valid: true,
    issues: [],
  };
}

function sessionView(overrides: Readonly<{
  queue?: QueueSnapshot;
  approvals?: readonly ApprovalSnapshot[];
  child_tasks?: readonly ChildTaskTreeItemSnapshot[];
}> = {}): SessionViewSnapshot {
  return {
    session: sessionSummary(),
    active_run: null,
    queue: overrides.queue ?? { revision: 1, state: "automatic", items: [] },
    approvals: { revision: 1, items: [...(overrides.approvals ?? [])], resolving_approval_id: null },
    attachments: [],
    file_references: [],
    runs: [],
    usage: { accumulated: null, previous_turn: null, context: null },
    child_tasks: [...(overrides.child_tasks ?? [])],
    conversation: {
      owner: { type: "main_session", session_id: "session-1" },
      generation: 1,
      items: [],
      previous_cursor: null,
      has_more: false,
    },
  };
}

function childTaskItem(): ChildTaskTreeItemSnapshot {
  return {
    task: {
      child_task_id: "child-1",
      session_id: "session-1",
      parent_run_id: "run-1",
      parent_tool_call_id: "call-delegate",
      title: "检查恢复一致性",
      status: "running",
      variant: "build",
      cancel_requested: false,
      final_text: "",
      error: null,
      created_at_ms: 1,
      started_at_ms: 2,
      finished_at_ms: null,
    },
    usage: { accumulated: null },
    pending_approval_count: 1,
    can_cancel: true,
  };
}

function sessionSummary(): SessionSummary {
  return {
    session_id: "session-1",
    title: "交互测试",
    model_key: "fixture",
    lifecycle: "active",
    current_variant: "build",
    approval_mode: "ask",
    workspace_id: "workspace-1",
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

function approvalSnapshot(): ApprovalSnapshot {
  const subject = {
    type: "shell" as const,
    tool_name: "shell",
    command: "npm run build && npm test",
    working_directory: "/workspace/project",
    timeout_ms: 60_000,
    process_mode: "foreground",
  };
  return {
    approval_id: "approval-1",
    session_id: "session-1",
    run_id: "run-1",
    call_id: "call-1",
    variant: "build",
    approval_mode: "ask",
    subject,
    available_decisions: ["allow_once", "allow_session", "allow_workspace", "deny"],
    exact_rule_preview: subject,
    status: "pending",
    created_at_ms: 1,
  };
}

function observed<T>(value: T, observed_sequence = 1): { observed_sequence: number; value: T } {
  return { observed_sequence, value };
}
