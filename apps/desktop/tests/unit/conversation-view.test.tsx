import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RuntimeEvent } from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { ConversationView } from "../../src/features/conversation/ConversationView";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ConversationView scroll anchoring", () => {
  it("renders user attachments above the message bubble", () => {
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 0,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        attachments: [{
          attachment_id: "attachment-1",
          session_id: "session-1",
          original_name: "参考图片.png",
          size_bytes: 1024,
          agent_readable_path: "/private/runtime/attachment-1.png",
          state: "ready",
          created_at_ms: 1,
        }],
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 1,
          items: [{
            type: "user",
            message_id: "message-with-attachment",
            text: "请查看这张图片",
            attachment_ids: ["attachment-1"],
            created_at_ms: 1,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    const attachment = screen.getByRole("button", { name: "参考图片.png" });
    const message = screen.getByText("请查看这张图片");
    expect(attachment.compareDocumentPosition(message) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
  });

  it("shows the completed turn finish time in the turn-level action bar", () => {
    const store = conversationStore();
    const finished_at_ms = Date.UTC(2026, 7, 14, 6, 25);
    store.projection.applySessionSnapshot({
      observed_sequence: 0,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 1,
          items: [{
            type: "assistant",
            message_id: "assistant-finished",
            run_id: "run-finished",
            attempt: 1,
            created_at_ms: null,
            finished_at_ms,
            status: "completed",
            segments: [{ type: "text", part_id: "text-finished", text: "完成内容" }],
            usage: null,
            can_fork: true,
            fork_point: "assistant-finished",
            feedback: null,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    const finished_at = new Date(finished_at_ms);
    const pad = (value: number) => String(value).padStart(2, "0");
    const expected = `${finished_at.getFullYear()}-${pad(finished_at.getMonth() + 1)}-${pad(finished_at.getDate())} ${pad(finished_at.getHours())}:${pad(finished_at.getMinutes())}:${pad(finished_at.getSeconds())}`;
    expect(screen.getByText(expected)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "赞同" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "不赞同" })).not.toBeInTheDocument();

    const collapse = screen.getByRole("button", { name: "收起消息" });
    expect(collapse.querySelector("svg")).toHaveAttribute("data-icon", "chevron-up");
    fireEvent.click(collapse);
    const expand = screen.getByRole("button", { name: "展开消息" });
    expect(expand.querySelector("svg")).toHaveAttribute("data-icon", "chevron-down");
  });

  it("previews and confirms a fork at the selected reliable assistant message", async () => {
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 0,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 7,
          items: [{
            type: "assistant",
            message_id: "assistant-fork",
            run_id: "run-fork",
            attempt: 1,
            created_at_ms: null,
            finished_at_ms: 2,
            status: "completed",
            segments: [{ type: "text", part_id: "text-fork", text: "分叉点" }],
            usage: null,
            can_fork: true,
            fork_point: "assistant-fork",
            feedback: null,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
    const fork = vi.spyOn(store, "forkSession").mockResolvedValue("session-forked");
    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    fireEvent.click(screen.getByRole("button", { name: "从此消息分叉" }));
    expect(screen.getByRole("dialog", { name: "从这条回复创建分支？" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "创建分支" }));

    await vi.waitFor(() => {
      expect(fork).toHaveBeenCalledWith("session-1", "assistant-fork", 7);
    });
  });

  it("follows streaming growth only while the user is pinned to the bottom", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = conversationStore();
    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);
    const scroll = screen.getByLabelText("消息列表");
    let scroll_height = 1_000;
    const client_height = 100;
    let scroll_top = 900;
    Object.defineProperties(scroll, {
      scrollHeight: { configurable: true, get: () => scroll_height },
      clientHeight: { configurable: true, get: () => client_height },
      scrollTop: {
        configurable: true,
        get: () => scroll_top,
        set: (value: number) => {
          scroll_top = Math.min(value, scroll_height - client_height);
        },
      },
    });
    fireEvent.scroll(scroll);

    scroll_height = 1_100;
    emitLive(store, [
      {
        type: "step_started",
        session_id: "session-1",
        run_id: "run-1",
        step: 1,
      },
      {
        type: "text_delta",
        session_id: "session-1",
        run_id: "run-1",
        part_id: "part-1",
        delta: "新增内容",
      },
    ], () => frame);
    expect(scroll_top).toBe(1_000);

    scroll_top = 400;
    fireEvent.scroll(scroll);
    scroll_height = 1_200;
    emitLive(store, [{
      type: "text_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "part-1",
      delta: "继续输出",
    }], () => frame);
    expect(scroll_top).toBe(400);
  });

  it("keeps committed and live steps in one turn without terminal actions", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 0,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 1,
          items: [{
            type: "assistant",
            message_id: "assistant-step-1",
            run_id: "run-1",
            attempt: 1,
            created_at_ms: null,
            finished_at_ms: null,
            status: "running",
            segments: [{
              type: "tool_group",
              tools: [{
                call_id: "call-1",
                tool_name: "read_file",
                status: "completed",
                summary: null,
              }],
            }],
            usage: null,
            can_fork: true,
            fork_point: "assistant-step-1",
            feedback: null,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
    emitLive(store, [
      { type: "run_started", session_id: "session-1", run_id: "run-1" },
      { type: "step_started", session_id: "session-1", run_id: "run-1", step: 2 },
      {
        type: "reasoning_delta",
        session_id: "session-1",
        run_id: "run-1",
        part_id: "reasoning-2",
        delta: "继续分析",
      },
      {
        type: "tool_proposed",
        session_id: "session-1",
        run_id: "run-1",
        call_id: "call-live-1",
        tool_name: "shell",
      },
      {
        type: "tool_proposed",
        session_id: "session-1",
        run_id: "run-1",
        call_id: "call-live-2",
        tool_name: "shell",
      },
      {
        type: "tool_output",
        session_id: "session-1",
        run_id: "run-1",
        call_id: "call-live-2",
        channel: "stdout",
        chunk: "SMOKE OK",
      },
      {
        type: "tool_completed",
        session_id: "session-1",
        run_id: "run-1",
        call_id: "call-live-1",
        status: "completed",
      },
      {
        type: "tool_completed",
        session_id: "session-1",
        run_id: "run-1",
        call_id: "call-live-2",
        status: "completed",
      },
    ], () => frame);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByRole("button", { name: /read file完成/ })).toBeInTheDocument();
    expect(screen.getByText("继续分析")).toBeInTheDocument();
    expect(screen.getByText("正在生成")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "复制助手正文" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "从此消息分叉" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "收起消息" })).not.toBeInTheDocument();

    const live_tools = screen.getAllByRole("button", { name: /shell完成/ });
    expect(live_tools).toHaveLength(2);
    fireEvent.click(live_tools[1]!);
    expect(screen.getByRole("dialog", { name: "shell" })).toBeInTheDocument();
    expect(screen.getByText("SMOKE OK")).toBeInTheDocument();
    expect(screen.getByText("当前为实时详情，可靠记录同步后可查看完整内容。")).toBeInTheDocument();
  });

  it("opens a formal child task branch from the parent turn", () => {
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 4,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        child_tasks: [childTaskItem()],
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 1,
          items: [{
            type: "assistant",
            message_id: "assistant-parent",
            run_id: "run-parent",
            attempt: 1,
            created_at_ms: 1,
            finished_at_ms: 2,
            status: "completed",
            segments: [{
              type: "tool_group",
              tools: [{
                call_id: "call-delegate",
                tool_name: "delegate_task",
                status: "completed",
                summary: null,
              }],
            }, { type: "text", part_id: "parent-text", text: "已派发子任务" }],
            usage: null,
            can_fork: true,
            fork_point: "assistant-parent",
            feedback: null,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    const tool = screen.getByRole("button", { name: /delegate task完成/ });
    const child_task = screen.getByRole("button", { name: "查看子任务：检查恢复一致性" });
    const actions = screen.getByRole("button", { name: "复制助手正文" });
    expect(tool.compareDocumentPosition(child_task) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(child_task.compareDocumentPosition(actions) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
    expect(screen.getByRole("region", { name: "子任务" })).toBeInTheDocument();

    fireEvent.click(child_task);

    expect(store.navigation.selected_child_task_id).toBe("child-1");
  });

  it("groups child-agent steps into one execution-level action bar", () => {
    const store = conversationStore();
    store.navigation.openChildTask("child-1");
    store.projection.applyChildTaskSnapshot({
      observed_sequence: 5,
      value: {
        task: childTaskItem(),
        approval_ids: [],
        conversation: {
          owner: { type: "child_task", session_id: "session-1", child_task_id: "child-1" },
          generation: 1,
          items: [{
            type: "assistant",
            message_id: "child-step-1",
            run_id: null,
            attempt: 1,
            created_at_ms: 1,
            finished_at_ms: 2,
            status: "completed",
            segments: [{ type: "reasoning", part_id: "reasoning-1", text: "先检查恢复路径" }],
            usage: null,
            can_fork: false,
            fork_point: null,
            feedback: null,
          }, {
            type: "assistant",
            message_id: "child-step-2",
            run_id: null,
            attempt: 1,
            created_at_ms: 3,
            finished_at_ms: 4,
            status: "completed",
            segments: [{
              type: "tool_group",
              tools: [{
                call_id: "child-call-1",
                tool_name: "shell",
                status: "completed",
                summary: null,
              }],
            }],
            usage: null,
            can_fork: false,
            fork_point: null,
            feedback: null,
          }, {
            type: "assistant",
            message_id: "child-step-3",
            run_id: null,
            attempt: 1,
            created_at_ms: 5,
            finished_at_ms: 6,
            status: "completed",
            segments: [{ type: "text", part_id: "child-result", text: "恢复检查完成" }],
            usage: null,
            can_fork: false,
            fork_point: null,
            feedback: null,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applyChildTaskSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    fireEvent.click(screen.getByRole("button", { name: "思考过程" }));
    expect(screen.getByText("先检查恢复路径")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /shell完成/ })).toBeInTheDocument();
    expect(screen.getByText("恢复检查完成")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "复制助手正文" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "收起消息" })).toHaveLength(1);
  });

  it("shows and opens a live child task before the session snapshot settles", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = conversationStore();

    emitLive(store, [{
      type: "child_task_event",
      session_id: "session-1",
      parent_run_id: "run-live-parent",
      child_task_id: "child-live",
      event: {
        type: "created",
        task: {
          child_task_id: "child-live",
          session_id: "session-1",
          parent_run_id: "run-live-parent",
          parent_tool_call_id: "call-delegate",
          title: "实时检查两个项目",
          status: "accepted",
          variant: "build",
          cancel_requested: false,
          final_text: "",
          error: null,
          created_at_ms: 2,
          started_at_ms: null,
          finished_at_ms: null,
        },
      },
    }], () => frame);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByText("当前主会话")).toBeVisible();
    const entry = screen.getByRole("button", { name: "查看子任务：实时检查两个项目" });
    fireEvent.click(entry);
    expect(store.navigation.selected_child_task_id).toBe("child-live");
  });

  it("keeps the empty child conversation content visible before messages arrive", () => {
    const store = conversationStore();
    store.navigation.openChildTask("child-1");
    store.projection.applyChildTaskSnapshot({
      observed_sequence: 5,
      value: {
        task: childTaskItem(),
        approval_ids: [],
        conversation: {
          owner: { type: "child_task", session_id: "session-1", child_task_id: "child-1" },
          generation: 0,
          items: [],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applyChildTaskSnapshot"]>[0]);
    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByText("子 Agent 尚未产生可展示的消息。")).toBeVisible();
  });
});

function childTaskItem() {
  return {
    task: {
      child_task_id: "child-1",
      session_id: "session-1",
      parent_run_id: "run-parent",
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
    usage: { accumulated: { input_tokens: 120, output_tokens: 30, cached_input_tokens: 80, total_tokens: 150 } },
    pending_approval_count: 0,
    can_cancel: true,
  } as const;
}

function conversationStore(): RootStore {
  const store = new RootStore();
  store.navigation.selectSession("session-1");
  store.projection.applySessionSnapshot({
    observed_sequence: 0,
    value: {
      session: { session_id: "session-1" },
      active_run: null,
      conversation: {
        owner: { type: "main_session", session_id: "session-1" },
        generation: 0,
        items: [{
          type: "user",
          message_id: "message-1",
          text: "开始",
          created_at_ms: 1,
        }],
        previous_cursor: null,
        has_more: false,
      },
    },
  } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
  return store;
}

function emitLive(
  store: RootStore,
  events: readonly RuntimeEvent[],
  getFrame: () => FrameRequestCallback | null,
): void {
  act(() => {
    events.forEach((event, index) => store.live_execution.buffer({
      sequence: index + 1,
      emitted_at_ms: index + 1,
      event,
    }));
    const callback = getFrame();
    callback?.(0);
  });
}
