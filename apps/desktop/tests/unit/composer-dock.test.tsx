import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApplicationSnapshot,
  ApprovalSnapshot,
  GoalSnapshot,
  QueueSnapshot,
  SessionSummary,
  SessionViewSnapshot,
  ChildTaskTreeItemSnapshot,
  WorkPlanSnapshot,
} from "../../src/generated/assistant-protocol";
import {
  chooseAttachmentFiles,
  previewAttachmentSelection,
  releaseAttachmentSelection,
  stageClipboardImage,
  uploadSelectedAttachment,
} from "../../src/native-bridge/nativeResource";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { ComposerDock } from "../../src/features/composer/ComposerDock";

vi.mock("../../src/native-bridge/nativeResource", () => ({
  NativeResourceFailure: class NativeResourceFailure extends Error {
    code: string | null = null;
  },
  cancelResourceOperation: vi.fn().mockResolvedValue(undefined),
  chooseAttachmentFiles: vi.fn(),
  materializeNewSession: vi.fn(),
  previewAttachment: vi.fn(),
  previewAttachmentSelection: vi.fn(),
  releaseAttachmentSelection: vi.fn().mockResolvedValue(undefined),
  stageClipboardImage: vi.fn(),
  uploadSelectedAttachment: vi.fn(),
}));

describe("ComposerDock", () => {
  it("shows queued and Goal MCP identities from reliable snapshots", () => {
    renderComposer({ goal: { ...goalSnapshot("paused"), mcp_server_key: "pencil" }, queue: {
      revision: 2, state: "automatic", items: [{ type: "message", payload: {
        input_id: "mcp-queue", text_preview: "正常消息", submitted_at_ms: 1, position: 1, is_prioritized: false,
        source: { type: "user" }, held_by_goal: true,
        mcp_selection: { server_key: "pencil", display_name: "Pencil" },
      } }],
    } });
    expect(screen.getByText("MCP · Pencil")).toHaveAttribute("title", "pencil");
    fireEvent.click(screen.getByRole("button", { name: /目标等待输入/ }));
    expect(within(screen.getByRole("region", { name: "目标详情" })).getByText("pencil")).toBeVisible();
  });

  it("selects an MCP by stable key, supports search/keyboard, and retains it after rejected submit", async () => {
    const user = userEvent.setup();
    const store = renderComposer();
    const list = vi.spyOn(store, "listMcpServerOptions").mockResolvedValue([
      { server_key: "github-a", display_name: "GitHub", description: "仓库一", visible_tool_count: 2 },
      { server_key: "github-b", display_name: "GitHub", description: "仓库二", visible_tool_count: 3 },
    ]);
    const submit = vi.spyOn(store, "submitInput").mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const command = vi.spyOn(store, "submitSessionCommand");
    const input = screen.getByRole("textbox", { name: "输入消息" });
    await user.type(input, "/mcp");
    await user.keyboard("{Enter}");
    const search = await screen.findByRole("combobox", { name: "搜索MCP 服务" });
    expect(search).toHaveFocus();
    expect(list).toHaveBeenCalledWith({ context: { type: "session", payload: { session_id: "session-1" } }, variant: "build" });
    expect(await screen.findByRole("option", { name: /GitHub \(github-a\)/ })).toBeVisible();
    expect(screen.getByRole("option", { name: /GitHub \(github-b\)/ })).toBeVisible();
    await user.type(search, "github-b");
    expect(screen.getAllByRole("option")).toHaveLength(1);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(input).toHaveFocus());
    expect(screen.getByText("MCP · GitHub")).toHaveAttribute("title", "github-b");
    expect(submit).not.toHaveBeenCalled();
    expect(command).not.toHaveBeenCalled();
    await user.type(input, "创建 issue");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(submit).toHaveBeenCalledWith("session-1", "创建 issue", "build", [], "normal", null, [], "github-b"));
    expect(screen.getByRole("button", { name: "移除 MCP GitHub" })).toBeVisible();
    expect(input).toHaveValue("创建 issue");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByText("MCP · GitHub")).not.toBeInTheDocument());
    expect(input).toHaveValue("");
  });

  it("clears MCP on variant/session changes and ignores a picker response after leaving its owner", async () => {
    const store = renderComposer();
    const server = { server_key: "pencil", display_name: "Pencil", description: "画图", visible_tool_count: 1 };
    const list = vi.spyOn(store, "listMcpServerOptions").mockResolvedValue([server]);
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/mcp" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.click(await screen.findByRole("option", { name: /Pencil/ }));
    act(() => store.projection.applySessionSnapshot(observed(sessionView({ session: { current_variant: "plan" } }))));
    expect(screen.queryByRole("button", { name: "移除 MCP Pencil" })).not.toBeInTheDocument();
    let resolve!: (servers: typeof server[]) => void;
    list.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    fireEvent.change(input, { target: { value: "/mcp" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(list).toHaveBeenLastCalledWith({ context: { type: "session", payload: { session_id: "session-1" } }, variant: "plan" });
    act(() => store.navigation.selectSession("missing-session"));
    await act(async () => resolve([server]));
    expect(screen.queryByRole("option", { name: /Pencil/ })).not.toBeInTheDocument();
  });

  it("retries failed MCP discovery and Escape restores composer focus", async () => {
    const store = renderComposer();
    vi.spyOn(store, "listMcpServerOptions").mockRejectedValueOnce(new Error("服务暂不可用")).mockResolvedValueOnce([]);
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/mcp" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(await screen.findByRole("alert")).toHaveTextContent("服务暂不可用");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText(/没有匹配的MCP 服务。/)).toBeVisible();
    expect(screen.getByRole("button", { name: "前往 MCP 设置" })).toBeVisible();
    fireEvent.keyDown(screen.getByRole("combobox", { name: "搜索MCP 服务" }), { key: "Escape" });
    await waitFor(() => expect(input).toHaveFocus());
  });

  it("queries new-session MCP options and clears only MCP on workspace change", async () => {
    const store = renderComposer();
    const list = vi.spyOn(store, "listMcpServerOptions").mockResolvedValue([{ server_key: "pencil", display_name: "Pencil", description: "画图", visible_tool_count: 1 }]);
    act(() => store.openNewSessionDraft("workspace-1"));
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/mcp" } });
    fireEvent.click(screen.getByRole("button", { name: "发送消息" }));
    fireEvent.click(await screen.findByRole("option", { name: /Pencil/ }));
    expect(list).toHaveBeenCalledWith({ context: { type: "new_session", payload: { workspace_id: "workspace-1" } }, variant: "build" });
    act(() => {
      store.new_session_drafts.updateGoalArmed("workspace:workspace-1", true);
      store.new_session_drafts.updateSelectedSkill("workspace:workspace-1", "review");
      store.openNewSessionDraft(null);
    });
    expect(store.new_session_drafts.get("workspace:workspace-1")).toMatchObject({ selected_mcp: null, selected_skill_name: "review", goal_armed: true });
    expect(screen.queryByText("MCP · Pencil")).not.toBeInTheDocument();
  });

  it("does not discard the idempotent first-send attempt when navigating after an unknown result", () => {
    const store = renderComposer();
    act(() => {
      store.openNewSessionDraft(null);
      store.new_session_drafts.updateSelectedMcp("unbound", { server_key: "pencil", display_name: "Pencil" });
      store.new_session_drafts.beginMaterialization("unbound", {
        idempotency_key: "unknown-send", message: "设计页面", variant: "build", approval_mode: "ask",
        mode: "normal", mcp_server_key: "pencil",
      });
    });
    act(() => store.openNewSessionDraft("workspace-1"));
    expect(store.new_session_drafts.get("unbound")).toMatchObject({
      selected_mcp: { server_key: "pencil" }, materialization_attempt: { idempotency_key: "unknown-send", mcp_server_key: "pencil" },
    });
  });

  it("keeps attachments above Goal/Skill/MCP tags and blocks tagged refresh commands", async () => {
    const store = renderComposer();
    vi.spyOn(store, "listMcpServerOptions").mockResolvedValue([{ server_key: "pencil", display_name: "Pencil", description: "画图", visible_tool_count: 1 }]);
    const command = vi.spyOn(store, "submitSessionCommand");
    vi.mocked(chooseAttachmentFiles).mockResolvedValue([{ selection_id: "order", original_name: "design.png", size_bytes: 10 }]);
    fireEvent.click(screen.getByRole("button", { name: "添加附件" }));
    const attachment = await screen.findByRole("button", { name: "查看附件 design.png" });
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/mcp" } });
    fireEvent.keyDown(input, { key: "Enter" });
    fireEvent.click(await screen.findByRole("option", { name: /Pencil/ }));
    const tag = screen.getByText("MCP · Pencil");
    expect(attachment.compareDocumentPosition(tag) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(tag.compareDocumentPosition(input) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    fireEvent.change(input, { target: { value: "/mcp refresh" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(screen.getByRole("alert")).toHaveTextContent("刷新指令不能同时携带");
    expect(command).not.toHaveBeenCalled();
  });

  it("shows MCP identity, full arguments and untrusted annotations for child approvals", () => {
    const arguments_json = JSON.stringify({ script: "x".repeat(8000) });
    renderComposer({ approvals: [{ ...approvalSnapshot(), child_task_id: "child-1", subject: {
      type: "mcp", identity: { server_key: "blender", server_display_name: "Blender", tool_name: "execute_code" },
      arguments_json, untrusted_annotations_json: '{"readOnlyHint":true}',
    } }] });
    expect(screen.getByText(/Blender \(blender\)/)).toBeVisible();
    expect(screen.getByText("子任务 · 由子智能体请求")).toBeVisible();
    expect(screen.getByText(arguments_json)).toHaveTextContent(arguments_json);
    expect(screen.getByText(/不能作为安全或只读保证/)).toBeVisible();
    fireEvent.click(screen.getByText("服务自报的工具注解（未经验证）"));
    expect(screen.getByText('{"readOnlyHint":true}')).toBeVisible();
  });

  it("submits MCP refresh as a command and keeps failed commands in the draft", async () => {
    const store = renderComposer();
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(true);
    const command = vi.spyOn(store, "submitSessionCommand").mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/mcp refresh github" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(command).toHaveBeenCalledWith("session-1", { type: "mcp_refresh", payload: { server: "github" } }));
    expect(input).toHaveValue("/mcp refresh github");
    expect(submit).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(input).toHaveValue(""));
  });

  it("keeps an executing refresh visible without run actions", () => {
    renderComposer({ queue: { revision: 2, state: "automatic", items: [{ type: "command", payload: {
      input_id: "refresh", command: { type: "mcp_refresh", payload: {} }, state: "executing",
      submitted_at_ms: 1, position: 1, is_prioritized: false,
    } }] } });
    expect(screen.getByText("MCP 刷新：全部")).toBeVisible();
    expect(screen.getByRole("status", { name: "正在刷新 MCP" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "移除" })).not.toBeInTheDocument();
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.restoreAllMocks();
  });

  it("keeps workspace drafts isolated and allows an attachment-only first send", async () => {
    const user = userEvent.setup();
    const store = new RootStore();
    store.connection.markConnected("instance-1", {
      protocol_version: 1,
      runtime_version: "test",
      max_command_bytes: 64 * 1024,
      max_attachment_bytes: null,
      sse: true,
      streaming_upload: true,
      features: ["session_materialization"],
    });
    store.projection.applyApplicationSnapshot(observed(applicationSnapshot()));
    store.openNewSessionDraft("workspace-1");
    const materialize = vi.spyOn(store, "materializeNewSessionDraft").mockResolvedValue(false);
    vi.mocked(chooseAttachmentFiles).mockResolvedValue([{
      selection_id: "selection-1",
      original_name: "screen.png",
      size_bytes: 12,
    }]);
    render(
      <RootStoreProvider store={store}>
        <ComposerDock />
      </RootStoreProvider>,
    );

    const input = screen.getByRole("textbox", { name: "输入消息" });
    await user.type(input, "工作区一草稿");
    act(() => store.openNewSessionDraft(null));
    expect(input).toHaveValue("");
    act(() => store.openNewSessionDraft("workspace-1"));
    expect(input).toHaveValue("工作区一草稿");

    fireEvent.change(input, { target: { value: "" } });
    await user.click(screen.getByRole("button", { name: "添加附件" }));
    expect(await screen.findByText("screen.png")).toBeVisible();
    expect(screen.getByRole("button", { name: "发送消息" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    expect(materialize).toHaveBeenCalledWith("workspace:workspace-1");
  });

  it("keeps pasted image attachments isolated between workspace drafts", async () => {
    const store = new RootStore();
    store.connection.markConnected("instance-1", {
      protocol_version: 1,
      runtime_version: "test",
      max_command_bytes: 64 * 1024,
      max_attachment_bytes: null,
      sse: true,
      streaming_upload: true,
      features: ["session_materialization"],
    });
    store.projection.applyApplicationSnapshot(observed(applicationSnapshot()));
    store.openNewSessionDraft("workspace-1");
    vi.mocked(stageClipboardImage).mockImplementation(async (file) => ({
      selection_id: `selection-${file.name}`,
      original_name: file.name,
      size_bytes: file.size,
      media_type: file.type,
      origin: "clipboard",
    }));
    render(
      <RootStoreProvider store={store}>
        <ComposerDock />
      </RootStoreProvider>,
    );
    const input = screen.getByRole("textbox", { name: "输入消息" });

    fireEvent(input, clipboardEvent([
      new File([new Uint8Array([1])], "workspace-one.png", { type: "image/png" }),
    ], ""));
    expect(await screen.findByRole("button", { name: "查看附件 workspace-one.png" })).toBeVisible();

    act(() => store.openNewSessionDraft(null));
    expect(screen.queryByRole("button", { name: "查看附件 workspace-one.png" })).not.toBeInTheDocument();
    fireEvent(input, clipboardEvent([
      new File([new Uint8Array([2])], "standalone.png", { type: "image/png" }),
    ], ""));
    expect(await screen.findByRole("button", { name: "查看附件 standalone.png" })).toBeVisible();

    act(() => store.openNewSessionDraft("workspace-1"));
    expect(await screen.findByRole("button", { name: "查看附件 workspace-one.png" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "查看附件 standalone.png" })).not.toBeInTheDocument();
  });

  it("routes an exact standalone /compact command without creating an input", async () => {
    const store = renderComposer();
    const compact = vi.spyOn(store, "compactSession").mockResolvedValue({
      type: "compacted",
      source_generation: 1,
      result_generation: 2,
      compacted_message_count: 6,
      retained_message_count: 2,
    });
    const submit = vi.spyOn(store, "submitInput");
    const input = screen.getByRole("textbox", { name: "输入消息" });

    fireEvent.change(input, { target: { value: "/compact" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(compact).toHaveBeenCalledWith("session-1", 1));
    expect(submit).not.toHaveBeenCalled();
    await waitFor(() => expect(input).toHaveValue(""));
  });

  it("routes an exact /title command through the title side path", async () => {
    const store = renderComposer();
    const generate_title = vi.spyOn(store, "generateSessionTitle").mockResolvedValue(true);
    const submit = vi.spyOn(store, "submitInput");
    const input = screen.getByRole("textbox", { name: "输入消息" });

    fireEvent.change(input, { target: { value: "/title" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(generate_title).toHaveBeenCalledWith("session-1"));
    expect(submit).not.toHaveBeenCalled();
    expect(input).toHaveValue("");
  });

  it("keeps title retry and close controls together without shrinking the retry label", async () => {
    const user = userEvent.setup();
    const store = renderComposer();
    const generate_title = vi.spyOn(store, "generateSessionTitle").mockResolvedValue(true);
    act(() => {
      store.session_notice = {
        session_id: "session-1",
        tone: "warning",
        message: "标题生成失败。",
        action: "retry_title",
      };
    });

    const retry = screen.getByRole("button", { name: "重试" });
    const close = screen.getByRole("button", { name: "关闭状态提示" });
    expect(retry.parentElement).toBe(close.parentElement);
    await user.click(retry);
    expect(generate_title).toHaveBeenCalledWith("session-1");
  });

  it("clears /compact when the session is busy", async () => {
    renderComposer({ session: { queued_input_count: 1 } });
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/compact" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(await screen.findByRole("alert")).toHaveTextContent("当前会话正忙");
    expect(input).toHaveValue("");
  });

  it("clears /compact when the Runtime rejects the operation", async () => {
    const store = renderComposer();
    const compact = vi.spyOn(store, "compactSession").mockResolvedValue(null);
    const input = screen.getByRole("textbox", { name: "输入消息" });
    fireEvent.change(input, { target: { value: "/compact" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(compact).toHaveBeenCalledWith("session-1", 1));
    expect(input).toHaveValue("");
  });

  it("reuses the primary stop action for manual compaction", async () => {
    const store = renderComposer({
      session: {
        active_compaction: {
          compaction_id: "compact-1",
          trigger: { type: "manual" },
          source_generation: 1,
          started_at_ms: 2,
          cancellable: true,
        },
      },
    });
    const cancel = vi.spyOn(store, "cancelSessionCompaction").mockResolvedValue(true);
    expect(screen.getByRole("textbox", { name: "输入消息" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "终止压缩" }));
    expect(cancel).toHaveBeenCalledWith("session-1", "compact-1");
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
    await waitFor(() => expect(submit).toHaveBeenCalledWith(
      "session-1",
      "第一行\n第二行",
      "build",
      [],
      "normal",
      null, [], null,
    ));
    await waitFor(() => expect(input).toHaveValue(""));

    await user.type(input, "/model");
    await user.keyboard("{Enter}");
    await user.click(await screen.findByRole("menuitemradio", { name: /备用模型/ }));
    await waitFor(() => expect(set_model).toHaveBeenCalledWith("session-1", "alternate"));
    await waitFor(() => expect(screen.queryByRole("menuitemradio", { name: /备用模型/ })).not.toBeInTheDocument());

    await user.type(input, "/mode");
    await user.keyboard("{Enter}");
    await user.click(await screen.findByRole("menuitemradio", { name: /规划/ }));
    expect(set_variant).toHaveBeenCalledWith("session-1", "plan");
  });

  it("does not submit the Enter used to finish input method composition", async () => {
    const store = renderComposer();
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(true);
    const input = screen.getByRole("textbox", { name: "输入消息" });

    fireEvent.compositionStart(input);
    fireEvent.change(input, { target: { value: "open ai" } });
    // WKWebView 可能先结束 composition，再发送用于确认本次输入的 Enter keydown。
    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Enter", keyCode: 13 });

    expect(submit).not.toHaveBeenCalled();
    expect(input).toHaveValue("open ai");

    fireEvent.keyUp(input, { key: "Enter", keyCode: 13 });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 13 });
    await waitFor(() => expect(submit).toHaveBeenCalledOnce());
  });

  it("recognizes the WebKit input method keyCode fallback", () => {
    const store = renderComposer();
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(true);
    const input = screen.getByRole("textbox", { name: "输入消息" });

    fireEvent.change(input, { target: { value: "raw phrase" } });
    fireEvent.keyDown(input, { key: "Enter", keyCode: 229 });

    expect(submit).not.toHaveBeenCalled();
    expect(input).toHaveValue("raw phrase");
  });

  it("supports queue priority, removal and explicit resume without a bottom-wide action", async () => {
    const user = userEvent.setup();
    const queue: QueueSnapshot = {
      revision: 4,
      state: "automatic",
      items: [
        { type: "message", payload: { input_id: "input-1", text_preview: "先检查构建", submitted_at_ms: 1, position: 1, is_prioritized: false, held_by_goal: false, source: { type: "user" } } },
        { type: "message", payload: { input_id: "input-2", text_preview: "再运行测试", submitted_at_ms: 2, position: 2, is_prioritized: false, held_by_goal: false, source: { type: "user" } } },
      ],
    };
    const store = renderComposer({ queue });
    const prioritize = vi.spyOn(store, "prioritizeQueuedInput").mockResolvedValue();
    const cancel = vi.spyOn(store, "cancelQueuedInput").mockResolvedValue();

    expect(screen.getByText("待执行队列", { exact: true })).toBeVisible();
    const queue_trigger = screen.getByRole("button", { name: /待执行队列/ });
    expect(queue_trigger).toHaveAttribute("aria-expanded", "true");
    await user.click(queue_trigger);
    expect(queue_trigger).toHaveAttribute("aria-expanded", "false");
    const exiting_queue = screen.getByText("再运行测试", { exact: true }).closest<HTMLElement>('[aria-hidden="true"]');
    expect(exiting_queue).not.toBeNull();
    if (exiting_queue) fireEvent.transitionEnd(exiting_queue);
    expect(screen.queryByText("再运行测试", { exact: true })).not.toBeInTheDocument();
    await user.click(queue_trigger);
    expect(screen.queryByText("先检查构建", { exact: true })).not.toBeInTheDocument();
    const first_row = screen.getByText("再运行测试").closest("div");
    expect(first_row).not.toBeNull();
    await user.click(within(first_row!).getByRole("button", { name: "优先" }));
    expect(prioritize).toHaveBeenCalledWith("session-1", "input-2", 4);
    await user.click(within(first_row!).getByRole("button", { name: "移除排队输入" }));
    expect(cancel).toHaveBeenCalledWith("session-1", "input-2");
    expect(screen.queryByRole("button", { name: /执行全部/ })).not.toBeInTheDocument();

    store.projection.applySessionSnapshot(observed(sessionView({
      queue: { ...queue, revision: 5, state: "paused_by_user" },
    }), 2));
    const resume = vi.spyOn(store, "resumeQueuedInput").mockResolvedValue();
    await user.click((await screen.findAllByRole("button", { name: "恢复" }))[0]!);
    expect(resume).toHaveBeenCalledWith("session-1", "input-1", 5);
  });

  it("selects exactly one skill, replaces it and clears it only after accepted submit", async () => {
    const user = userEvent.setup();
    const skill_catalog: SessionViewSnapshot["skill_catalog"] = {
      status: "ready",
      diagnostics: [],
      skills: [
        { name: "review", description: "检查代码", source: "workspace_ez_assistant", model_invocable: true, user_invocable: true, enabled: true, health: "ready" },
        { name: "release", description: "准备发布", source: "workspace_agents", model_invocable: false, user_invocable: true, enabled: true, health: "ready" },
      ],
    };
    const store = renderComposer({ skill_catalog });
    const submit = vi.spyOn(store, "submitInput").mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const input = screen.getByRole("textbox", { name: "输入消息" });

    await user.type(input, "/skill");
    fireEvent.keyDown(input, { key: "Enter" });
    await user.click(screen.getByRole("option", { name: /review/ }));
    expect(screen.getByRole("button", { name: "移除技能 review" })).toBeVisible();

    await user.type(input, "/skill");
    fireEvent.keyDown(input, { key: "Enter" });
    await user.click(screen.getByRole("option", { name: /release/ }));
    expect(screen.queryByRole("button", { name: "移除技能 review" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "移除技能 release" })).toBeVisible();

    await user.type(input, "准备当前版本");
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(submit).toHaveBeenLastCalledWith(
      "session-1", "准备当前版本", "build", [], "normal", "release", [], null,
    ));
    expect(screen.getByRole("button", { name: "移除技能 release" })).toBeVisible();
    expect(input).toHaveValue("准备当前版本");

    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(screen.queryByRole("button", { name: "移除技能 release" })).not.toBeInTheDocument());
    expect(input).toHaveValue("");
  });

  it("lets a pending approval take over the bottom workspace and remain blocking when minimized", async () => {
    const user = userEvent.setup();
    const approval = approvalSnapshot();
    const queue: QueueSnapshot = {
      revision: 2,
      state: "automatic",
      items: [{ type: "message", payload: {
        input_id: "queued-during-approval",
        text_preview: "审批后执行下一项",
        submitted_at_ms: 2,
        position: 1,
        is_prioritized: false,
        held_by_goal: false,
        source: { type: "user" },
      } }],
    };
    const store = renderComposer({ approvals: [approval], queue });
    const decide = vi.spyOn(store, "decideApproval").mockResolvedValue();
    const reject_and_stop = vi.spyOn(store, "rejectApprovalAndStopRun").mockResolvedValue();

    expect(screen.getByText("允许执行命令行指令？", { exact: true })).toBeVisible();
    expect(screen.queryByText("审批后执行下一项", { exact: true })).not.toBeInTheDocument();
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
    expect(reject_and_stop).toHaveBeenCalledWith("session-1", "approval-1", 2);
  });

  it("uses the parent approval workspace for a child task while keeping the child view read-only", () => {
    const approval = { ...approvalSnapshot(), child_task_id: "child-1" };
    renderComposer({ approvals: [approval], child_tasks: [childTaskItem()], read_only: true });

    expect(screen.getByText("检查恢复一致性 · 由子智能体请求")).toBeVisible();
    expect(screen.getByText("允许执行命令行指令？", { exact: true })).toBeVisible();
    expect(screen.queryByRole("textbox", { name: "输入消息" })).not.toBeInTheDocument();
  });

  it("shows every resolved path for a multi-file read approval", () => {
    const subject = {
      type: "files" as const,
      tool_name: "inspect_images",
      operation: "read",
      paths: ["/workspace/a.png", "/tmp/b.png"],
    };
    renderComposer({
      approvals: [{
        ...approvalSnapshot(),
        subject,
        exact_rule_preview: subject,
      }],
    });

    expect(screen.getByText("允许执行 inspect_images？", { exact: true })).toBeVisible();
    expect(screen.getByText("read · 2 个文件", { exact: true })).toBeVisible();
    expect(screen.getByText("/workspace/a.png", { exact: true })).toBeVisible();
    expect(screen.getByText("/tmp/b.png", { exact: true })).toBeVisible();
    expect(screen.getAllByText("当前会话", { exact: true })).toHaveLength(2);
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
    expect(screen.queryByText("指令帮助")).not.toBeInTheDocument();
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
      "normal",
      null, [], null,
    ));
    expect(screen.queryByText("notes.md")).not.toBeInTheDocument();
  });

  it("opens attachment details from the tag body while keeping remove separate", async () => {
    const user = userEvent.setup();
    renderComposer();
    vi.mocked(chooseAttachmentFiles).mockResolvedValue([{
      selection_id: "selection-detail",
      original_name: "notes.md",
      size_bytes: 2048,
      media_type: null,
      origin: "file_picker",
    }]);
    vi.mocked(previewAttachmentSelection).mockResolvedValue({
      kind: "text",
      media_type: "text/markdown",
      size_bytes: 2048,
      text: "# Preview",
      data_url: null,
    });

    await user.click(screen.getByRole("button", { name: "添加附件" }));
    await user.click(await screen.findByRole("button", { name: "查看附件 notes.md" }));
    expect(await screen.findByRole("dialog", { name: "notes.md" })).toBeVisible();
    expect(await screen.findByText("# Preview")).toBeVisible();
    expect(screen.getByText("text/markdown")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "关闭附件详情" }));

    await user.click(screen.getByRole("button", { name: "移除附件 notes.md" }));
    expect(screen.queryByRole("button", { name: "查看附件 notes.md" })).not.toBeInTheDocument();
    expect(releaseAttachmentSelection).toHaveBeenCalledWith("selection-detail");
  });

  it("keeps pure text paste native and inserts mixed clipboard text at the current selection", async () => {
    renderComposer();
    const input = screen.getByRole("textbox", { name: "输入消息" }) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "before after" } });
    input.setSelectionRange(7, 12);

    const pure_text = clipboardEvent([], "native text");
    fireEvent(input, pure_text);
    expect(pure_text.defaultPrevented).toBe(false);
    expect(stageClipboardImage).not.toHaveBeenCalled();

    const image = new File([new Uint8Array([1, 2, 3])], "shot.png", { type: "image/png" });
    vi.mocked(stageClipboardImage).mockResolvedValue({
      selection_id: "selection-paste",
      original_name: "shot.png",
      size_bytes: 3,
      media_type: "image/png",
      origin: "clipboard",
    });
    const mixed = clipboardEvent([image], "middle");
    fireEvent(input, mixed);

    expect(mixed.defaultPrevented).toBe(true);
    expect(input).toHaveValue("before middle");
    expect(await screen.findByRole("button", { name: "查看附件 shot.png" })).toBeVisible();
    expect(stageClipboardImage).toHaveBeenCalledWith(image);
  });

  it("rolls back a clipboard image batch when staging fails midway", async () => {
    renderComposer();
    const input = screen.getByRole("textbox", { name: "输入消息" }) as HTMLTextAreaElement;
    const first = new File([new Uint8Array([1])], "first.png", { type: "image/png" });
    const second = new File([new Uint8Array([2])], "second.png", { type: "image/png" });
    vi.mocked(stageClipboardImage)
      .mockResolvedValueOnce({
        selection_id: "selection-first",
        original_name: "first.png",
        size_bytes: 1,
        media_type: "image/png",
        origin: "clipboard",
      })
      .mockRejectedValueOnce(new Error("图片内容无效"));

    fireEvent(input, clipboardEvent([first, second], "保留这段文字"));
    expect(input).toHaveValue("保留这段文字");
    await waitFor(() => expect(releaseAttachmentSelection).toHaveBeenCalledWith("selection-first"));
    expect(screen.queryByRole("button", { name: /查看附件 first\.png/ })).not.toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("图片内容无效");
  });

  it("keeps multiple pasted images ordered and rejects a batch beyond composer capacity", async () => {
    renderComposer();
    const input = screen.getByRole("textbox", { name: "输入消息" }) as HTMLTextAreaElement;
    const first = new File([new Uint8Array([1])], "first.png", { type: "image/png" });
    const second = new File([new Uint8Array([2])], "second.png", { type: "image/png" });
    vi.mocked(stageClipboardImage).mockImplementation(async (file) => ({
      selection_id: `selection-${file.name}`,
      original_name: file.name,
      size_bytes: file.size,
      media_type: file.type,
      origin: "clipboard",
    }));

    fireEvent(input, clipboardEvent([first, second], ""));
    await waitFor(() => expect(screen.getAllByRole("button", { name: /查看附件/ })).toHaveLength(2));
    expect(screen.getAllByRole("button", { name: /查看附件/ }).map((button) => button.textContent)).toEqual([
      expect.stringContaining("first.png"),
      expect.stringContaining("second.png"),
    ]);

    vi.mocked(stageClipboardImage).mockClear();
    const overflow = Array.from({ length: 31 }, (_, index) => (
      new File([new Uint8Array([index])], `extra-${index}.png`, { type: "image/png" })
    ));
    fireEvent(input, clipboardEvent(overflow, "文字仍保留"));
    expect(input).toHaveValue("文字仍保留");
    expect(stageClipboardImage).not.toHaveBeenCalled();
    expect(await screen.findByRole("alert")).toHaveTextContent("每条消息最多添加 32 个附件");
    expect(screen.getAllByRole("button", { name: /查看附件/ })).toHaveLength(2);
  });

  it("allows an attachment-only input in an existing session", async () => {
    const user = userEvent.setup();
    const store = renderComposer();
    vi.mocked(chooseAttachmentFiles).mockResolvedValue([{
      selection_id: "selection-image-only",
      original_name: "image.png",
      size_bytes: 3,
    }]);
    vi.mocked(uploadSelectedAttachment).mockResolvedValue({
      attachment: {
        attachment_id: "attachment-image-only",
        session_id: "session-1",
        original_name: "image.png",
        size_bytes: 3,
        agent_readable_path: "/private/runtime/attachment-image-only",
        state: "ready",
        created_at_ms: 1,
      },
    });
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(true);

    await user.click(screen.getByRole("button", { name: "添加附件" }));
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    await waitFor(() => expect(submit).toHaveBeenCalledWith(
      "session-1",
      "",
      "build",
      ["attachment-image-only"],
      "normal",
      null, [], null,
    ));
  });

  it("uses the model cascade and keeps image handling out of the composer", async () => {
    const user = userEvent.setup();
    const store = renderComposer({
      composer_capabilities: {
        selected_model_key: "fixture",
        reasoning_effort_options: [
          { key: "low", label: "较少" },
          { key: "max", label: "最大" },
        ],
        image_handling: "tool",
        goal_supported: true,
      },
    });
    const set_effort = vi.spyOn(store, "setSessionReasoningEffort").mockResolvedValue(true);

    expect(screen.queryByText("工具识图")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "模型设置" }));
    await user.click(screen.getByRole("menuitem", { name: /推理强度/ }));
    await user.click(screen.getByRole("menuitemradio", { name: /最大/ }));
    expect(set_effort).toHaveBeenCalledWith("session-1", "max");
  });

  it("positions both settings overlays after their Presence content mounts", async () => {
    const user = userEvent.setup();
    renderComposer();

    await user.click(screen.getByRole("button", { name: "执行设置" }));
    await expectPositionedOverlay("执行设置");

    await user.click(screen.getByRole("button", { name: "模型设置" }));
    await expectPositionedOverlay("模型设置");
  });

  it("shows the context percentage and token usage in a positioned tooltip", async () => {
    renderComposer({
      context_usage: {
        used_tokens: 52_000,
        window_tokens: 1_000_000,
        usage_basis_points: 520,
      },
    });

    const ring = screen.getByRole("img", { name: "上下文用量：52K / 1000K · 5.2%" });
    fireEvent.focus(ring);
    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("52K / 1000K · 5.2%");
    await waitFor(() => expect(tooltip).toHaveAttribute("data-position-ready", "true"));
  });

  it("loads a missing historical model as unselected and blocks messages until reselection", async () => {
    const user = userEvent.setup();
    const store = renderComposer({
      composer_capabilities: {
        selected_model_key: null,
        reasoning_effort_options: [],
        image_handling: "unavailable",
        goal_supported: false,
      },
    });
    const set_model = vi.spyOn(store, "setSessionModel").mockResolvedValue(true);
    const input = screen.getByRole("textbox", { name: "输入消息" });

    expect(input).toBeDisabled();
    expect(input).toHaveAttribute("placeholder", "请先选择一个可用模型");
    expect(screen.getByRole("button", { name: "模型设置" })).toHaveTextContent("未选择模型");

    await user.click(screen.getByRole("button", { name: "模型设置" }));
    await user.click(screen.getByRole("menuitem", { name: /模型/ }));
    expect(screen.getAllByRole("menuitemradio").every(
      (item) => item.getAttribute("aria-checked") === "false",
    )).toBe(true);
    await user.click(screen.getByRole("menuitemradio", { name: /备用模型/ }));
    expect(set_model).toHaveBeenCalledWith("session-1", "alternate");
  });

  it("repositions the settings submenu from measured DOM dimensions", async () => {
    const user = userEvent.setup();
    let viewport_height = 600;
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(1_000);
    vi.spyOn(window, "innerHeight", "get").mockImplementation(() => viewport_height);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function measuredRect(this: HTMLElement) {
      if (this instanceof HTMLDivElement && this.getAttribute("aria-label") === "模型设置") {
        return domRect(766, 300, 226, 100);
      }
      if (this.getAttribute("role") === "menu" && this.getAttribute("aria-label") === "模型") {
        return domRect(0, 0, 340, 330);
      }
      return domRect(0, 0, 0, 0);
    });
    renderComposer();

    await user.click(screen.getByRole("button", { name: "模型设置" }));
    await user.click(screen.getByRole("menuitem", { name: /^模型/ }));
    const secondary = await screen.findByRole("menu", { name: "模型" });
    await waitFor(() => {
      expect(secondary).toHaveAttribute("data-side", "left");
      expect(secondary).toHaveStyle({ top: "-38px" });
    });

    viewport_height = 700;
    fireEvent(window, new Event("resize"));
    await waitFor(() => expect(secondary).toHaveStyle({ top: "-6px" }));
  });

  it("arms /goal once, ignores Escape and preserves the tag after a failed submit", async () => {
    const user = userEvent.setup();
    const store = renderComposer({
      composer_capabilities: {
        selected_model_key: "fixture",
        reasoning_effort_options: [],
        image_handling: "unavailable",
        goal_supported: false,
      },
    });
    const submit = vi.spyOn(store, "submitInput").mockResolvedValue(false);
    const input = screen.getByRole("textbox", { name: "输入消息" });

    await user.type(input, "/goal");
    await user.keyboard("{Enter}");
    expect(input).toHaveValue("");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "取消目标标记" })).toBeVisible();
    await user.keyboard("{Escape}");
    expect(screen.getByRole("button", { name: "取消目标标记" })).toBeVisible();

    await user.type(input, "完成本版本验收");
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    await waitFor(() => expect(submit).toHaveBeenCalledWith(
      "session-1",
      "完成本版本验收",
      "build",
      [],
      "start_goal",
      null, [], null,
    ));
    expect(input).toHaveValue("完成本版本验收");
    await user.click(screen.getByRole("button", { name: "取消目标标记" }));
    expect(input).toHaveValue("完成本版本验收");
  });

  it("shows Todo independently and exposes paused Goal resume and exit actions", async () => {
    const user = userEvent.setup();
    const store = renderComposer({ work_plan: workPlan(), goal: goalSnapshot("paused") });
    const resume_goal = vi.spyOn(store, "resumeGoal").mockResolvedValue(true);
    const clear_goal = vi.spyOn(store, "clearGoal").mockResolvedValue(true);

    const todo_summary = screen.getByRole("button", { name: /工作计划/ });
    expect(todo_summary).toHaveTextContent("编写客户端交互");
    expect(todo_summary).not.toHaveTextContent("完成 v0.18.0");
    expect(todo_summary).not.toHaveTextContent("Todo");
    expect(todo_summary.querySelector("span[aria-hidden='true']")).not.toBeInTheDocument();
    await user.hover(todo_summary);
    const todo_dialog = await screen.findByRole("dialog", { name: "工作计划详情" });
    expect(todo_dialog).toBeVisible();
    expect(within(todo_dialog).queryByText("总体目标")).not.toBeInTheDocument();
    expect(within(todo_dialog).getByText("编写客户端交互", { exact: true })).toBeVisible();
    expect(within(todo_dialog).queryByRole("button")).not.toBeInTheDocument();
    await user.unhover(todo_summary);
    expect(document.querySelector("[data-todo-detail]")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "工作计划详情" })).not.toBeInTheDocument());
    await waitFor(() => expect(document.querySelector("[data-todo-detail]")).not.toBeInTheDocument());

    const goal_trigger = screen.getByRole("button", { name: /目标等待输入/ });
    expect(goal_trigger).toHaveAttribute("aria-expanded", "false");
    await user.click(goal_trigger);
    const goal_detail = screen.getByRole("region", { name: "目标详情" });
    expect(goal_detail).toBeVisible();
    expect(goal_trigger).toHaveAttribute("aria-expanded", "true");
    expect(goal_trigger.closest("section")).toContainElement(goal_detail);
    expect(screen.queryByRole("dialog", { name: "Goal 详情" })).not.toBeInTheDocument();
    expect(within(goal_detail).queryByText("Goal 目标")).not.toBeInTheDocument();
    expect(within(goal_detail).queryByText(/世代/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "模型设置" }));
    expect(goal_detail).toBeVisible();
    await user.click(screen.getByRole("button", { name: "模型设置" }));
    await user.click(screen.getByRole("button", { name: "继续" }));
    await waitFor(() => expect(resume_goal).toHaveBeenCalledWith("session-1", "goal-1", 2));

    await user.click(screen.getByRole("button", { name: "退出目标" }));
    expect(clear_goal).not.toHaveBeenCalled();
    const exit_dialog = screen.getByRole("dialog", { name: "退出当前目标？" });
    await user.click(within(exit_dialog).getByRole("button", { name: "退出目标" }));
    await waitFor(() => expect(clear_goal).toHaveBeenCalledWith("session-1", "goal-1", 2));
  });

  it("shows Todo loading only while the session has an active Run", () => {
    renderComposer({
      work_plan: workPlan(),
      session: { active_run_id: "run-1", active_run_status: "running" },
    });

    const todo_summary = screen.getByRole("button", { name: /工作计划/ });
    expect(todo_summary.querySelector("span[aria-hidden='true']")).toBeInTheDocument();
  });

  it("treats a work plan with no items as a cleared Todo list", () => {
    renderComposer({ work_plan: { ...workPlan(), items: [] } });

    expect(screen.queryByRole("button", { name: /工作计划/ })).not.toBeInTheDocument();
    expect(screen.queryByText("工作计划待更新")).not.toBeInTheDocument();
  });

  it("centers the wider Todo detail from measured trigger and overlay dimensions", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "innerWidth", "get").mockReturnValue(1_000);
    vi.spyOn(window, "innerHeight", "get").mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function measuredRect(this: HTMLElement) {
      if (this instanceof HTMLButtonElement && this.getAttribute("aria-label")?.startsWith("工作计划：")) {
        return domRect(320, 700, 360, 26);
      }
      if (this instanceof HTMLDivElement && this.getAttribute("aria-label") === "工作计划详情") {
        return domRect(0, 0, 640, 240);
      }
      return domRect(0, 0, 0, 0);
    });
    renderComposer({ work_plan: workPlan() });

    await user.click(screen.getByRole("button", { name: /工作计划/ }));
    const dialog = await screen.findByRole("dialog", { name: "工作计划详情" });
    await waitFor(() => expect(dialog.parentElement).toHaveStyle({ left: "180px", top: "454px" }));
    await waitFor(() => expect(document.querySelector("[data-todo-message-mask]"))
      .toHaveStyle({ top: "402px" }));
  });

  it("keeps Goal-held guidance queued until a paused Goal explicitly consumes it", async () => {
    const user = userEvent.setup();
    const queue: QueueSnapshot = {
      revision: 7,
      state: "resume_required",
      items: [{ type: "message", payload: {
        input_id: "input-held",
        text_preview: "补充检查边界情况",
        submitted_at_ms: 1,
        position: 1,
        is_prioritized: false,
        held_by_goal: true,
        source: { type: "user" },
      } }],
    };
    const store = renderComposer({ queue, goal: goalSnapshot("paused") });
    const resume_goal = vi.spyOn(store, "resumeGoal").mockResolvedValue(true);

    const queue_trigger = screen.getByRole("button", { name: /待处理指导/ });
    const goal_trigger = screen.getByRole("button", { name: /目标等待输入/ });
    expect(queue_trigger).toHaveAttribute("aria-expanded", "true");
    expect(goal_trigger).toHaveAttribute("aria-expanded", "false");
    await user.click(goal_trigger);
    expect(goal_trigger).toHaveAttribute("aria-expanded", "true");
    expect(queue_trigger).toHaveAttribute("aria-expanded", "false");
    await user.click(queue_trigger);
    expect(queue_trigger).toHaveAttribute("aria-expanded", "true");
    expect(goal_trigger).toHaveAttribute("aria-expanded", "false");

    expect(screen.queryByRole("button", { name: "确认并继续全部" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "用于目标" }));
    expect(resume_goal).toHaveBeenCalledWith("session-1", "goal-1", 2, "input-held");
  });

  it("stops a running Goal from the primary action and disables duplicate /goal", async () => {
    const user = userEvent.setup();
    const store = renderComposer({
      goal: goalSnapshot("running"),
      composer_capabilities: {
        selected_model_key: "fixture",
        reasoning_effort_options: [],
        image_handling: "unavailable",
        goal_supported: true,
      },
    });
    const stop_goal = vi.spyOn(store, "stopGoal").mockResolvedValue(true);

    await user.click(screen.getByRole("button", { name: "停止目标" }));
    await waitFor(() => expect(stop_goal).toHaveBeenCalledWith("session-1", "goal-1", 2));

    const input = screen.getByRole("textbox", { name: "输入消息" });
    await user.type(input, "/goal");
    expect(screen.getByText("当前会话已有目标，请先继续或退出现有目标")).toBeVisible();
    await user.keyboard("{Enter}");
    expect(screen.getByRole("alert")).toHaveTextContent("当前会话已有目标");
    expect(input).toHaveValue("");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "取消目标标记" })).not.toBeInTheDocument();
  });

  it("offers only online reply channels with category icons and no subtitles", async () => {
    const user = userEvent.setup();
    const store = renderComposer({ session: { role: "controller" } });
    store.device_gateway.stale = false;
    store.device_gateway.snapshot = {
      enabled: true,
      available: true,
      installation_id: "installation-1",
      certificate_fingerprint: "fingerprint",
      pending_pairings: [],
      devices: [{
        device_id: "device-1",
        display_name: "客厅终端",
        lifecycle: "paired",
        paired_at_ms: 1,
        updated_at_ms: 1,
        revoked_at_ms: null,
        connection: {
          connected_at_ms: 2,
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
      }, {
        device_id: "device-2",
        display_name: "离线终端",
        lifecycle: "paired",
        paired_at_ms: 1,
        updated_at_ms: 1,
        revoked_at_ms: null,
      }],
      speech_services: { asr: "unavailable", tts: "unavailable" },
    };
    const set_hosting = vi.spyOn(store.device_gateway, "setOutputHosting").mockResolvedValue(true);

    const trigger = screen.getByRole("button", { name: "选择回复频道" });
    expect(trigger.querySelector("svg")).toHaveAttribute("data-icon", "channel");
    await user.click(trigger);

    const menu = screen.getByRole("menu", { name: "选择回复频道" });
    const desktop = within(menu).getByRole("menuitem", { name: "仅在 Desktop 显示" });
    const device = within(menu).getByRole("menuitem", { name: "客厅终端" });
    expect(desktop.querySelector("svg")).toHaveAttribute("data-icon", "desktop");
    expect(device.querySelector("svg")).toHaveAttribute("data-icon", "device");
    expect(within(menu).queryByRole("menuitem", { name: "离线终端" })).not.toBeInTheDocument();
    expect(within(menu).getByRole("menuitem", { name: "管理智能终端" })).toBeVisible();
    expect(menu.querySelector("small")).not.toBeInTheDocument();

    await user.click(device);
    expect(set_hosting).toHaveBeenCalledWith("device-1");
  });
});

function renderComposer(overrides: Readonly<{
  queue?: QueueSnapshot;
  approvals?: readonly ApprovalSnapshot[];
  child_tasks?: readonly ChildTaskTreeItemSnapshot[];
  context_usage?: SessionViewSnapshot["usage"]["context"];
  read_only?: boolean;
  composer_capabilities?: SessionViewSnapshot["composer_capabilities"];
  goal?: GoalSnapshot | null;
  work_plan?: WorkPlanSnapshot | null;
  skill_catalog?: SessionViewSnapshot["skill_catalog"];
  session?: Partial<SessionSummary>;
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
  render(
    <>
      <div data-conversation-area>
        <RootStoreProvider store={store}>
          <ComposerDock read_only={overrides.read_only} />
        </RootStoreProvider>
      </div>
      <div id="overlay-root" />
    </>,
  );
  return store;
}

async function expectPositionedOverlay(label: string): Promise<void> {
  await waitFor(() => {
    const overlay = document.querySelector<HTMLElement>(`#overlay-root > [aria-label="${label}"]`);
    expect(overlay).not.toBeNull();
    expect(overlay).toHaveAttribute("data-position-ready", "true");
    expect(overlay).toHaveAttribute("data-presence", "entered");
  });
}

function applicationSnapshot(): ApplicationSnapshot {
  return {
    runtime_lifecycle: "running",
    configuration: { config_path: null, revision: "fixture-revision", state: "ready", schema_version: 1, default_model: "fixture", auxiliary_vision_model: null, issues: [] },
    models: [
      model("fixture", "本地模型（7B）", true),
      model("alternate", "备用模型", false),
    ],
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
    active_sessions: [sessionSummary()],
    archived_sessions: [],
    controller_availability: { status: "unavailable" },
    additional_controller_count: 0,
    capabilities: { conversation_paging: true, mcp_tools: true, mcp_management: true, session_commands: true, tool_detail: true, queue_control: true, approval_queue: true, child_task_view: true, conversation_search: true },
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
    supports_image_input: false,
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
  context_usage?: SessionViewSnapshot["usage"]["context"];
  composer_capabilities?: SessionViewSnapshot["composer_capabilities"];
  goal?: GoalSnapshot | null;
  work_plan?: WorkPlanSnapshot | null;
  skill_catalog?: SessionViewSnapshot["skill_catalog"];
  session?: Partial<SessionSummary>;
}> = {}): SessionViewSnapshot {
  return {
    session: { ...sessionSummary(), ...overrides.session },
    conversation_generation: 1,
    composer_capabilities: overrides.composer_capabilities ?? {
      selected_model_key: "fixture",
      reasoning_effort_options: [],
      image_handling: "unavailable",
      goal_supported: false,
    },
    work_plan: overrides.work_plan ?? null,
    goal: overrides.goal ?? null,
    active_run: null,
    queue: overrides.queue ?? { revision: 1, state: "automatic", items: [] },
    approvals: { revision: 1, items: [...(overrides.approvals ?? [])], resolving_approval_id: null },
    attachments: [],
    file_references: [],
    runs: [],
    usage: {
      accumulated: null,
      previous_turn: null,
      latest_cache_hit_basis_points: null,
      overall_cache_hit_basis_points: null,
      context: overrides.context_usage ?? null,
    },
    child_tasks: [...(overrides.child_tasks ?? [])],
    skill_catalog: overrides.skill_catalog ?? {
      status: "empty",
      skills: [],
      diagnostics: [],
    },
    active_skills: [],
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
    role: "standard",
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

function workPlan(): WorkPlanSnapshot {
  return {
    revision: 3,
    objective: "完成 v0.18.0",
    items: [
      { id: "todo-1", text: "实现 Runtime", status: "completed" },
      { id: "todo-2", text: "编写客户端交互", status: "in_progress" },
      { id: "todo-3", text: "完成验收", status: "pending" },
    ],
    updated_at_ms: 2,
  };
}

function goalSnapshot(state: GoalSnapshot["state"]): GoalSnapshot {
  return {
    goal_id: "goal-1",
    objective_message_id: "message-1",
    objective_preview: "完成 v0.18.0 自动续跑",
    attachment_count: 1,
    state,
    pause_reason: state === "paused" ? { type: "blocked", summary: "需要确认客户端行为" } : null,
    generation: 2,
    turn: 4,
    budget: {
      max_runs: 20,
      max_total_tokens: 200_000,
      max_consecutive_failures: 3,
      used_runs: 4,
      used_total_tokens: 42_000,
      consecutive_failures: 0,
      usage_complete: true,
    },
    created_at_ms: 1,
    updated_at_ms: 2,
    completed_at_ms: state === "completed" ? 2 : null,
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

function clipboardEvent(files: readonly File[], text: string): ClipboardEvent {
  const event = new Event("paste", { bubbles: true, cancelable: true }) as ClipboardEvent;
  Object.defineProperty(event, "clipboardData", {
    value: {
      getData: (format: string) => format === "text/plain" ? text : "",
      items: files.map((file) => ({
        getAsFile: () => file,
        kind: "file",
        type: file.type,
      })),
    },
  });
  return event;
}

function domRect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    bottom: top + height,
    height,
    left,
    right: left + width,
    top,
    width,
    x: left,
    y: top,
    toJSON: () => ({}),
  };
}
