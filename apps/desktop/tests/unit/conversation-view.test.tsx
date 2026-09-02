import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RuntimeEvent } from "../../src/generated/assistant-protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";
import { ConversationView } from "../../src/features/conversation/ConversationView";
import { UserMessage } from "../../src/features/conversation/ConversationView/MessageViews";
import { thumbnailAttachment } from "../../src/native-bridge/nativeResource";

vi.mock("../../src/native-bridge/nativeResource", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../src/native-bridge/nativeResource")>()),
  thumbnailAttachment: vi.fn(),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ConversationView scroll anchoring", () => {
  it("renders structured controller delivery and proxy report metadata", () => {
    const source_session = {
      session_id: "source-1",
      title: "来源会话",
      lifecycle: "active",
    } as import("../../src/generated/assistant-protocol").SessionSummary;
    const open = vi.fn();
    const { rerender } = render(
      <UserMessage
        attachments={[]}
        message={{
          message_id: "delivery-1",
          input_id: null,
          text: "继续检查构建",
          attachment_ids: [],
          quotes: [],
          source: { type: "controller_delivery", controller_session_id: "source-1", controller_run_id: "run-1" },
          created_at_ms: 1,
        }}
        on_attachment_click={vi.fn()}
        on_source_open={open}
        source_session={source_session}
      />,
    );
    expect(screen.getByText("主控转达")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "来源会话" }));
    expect(open).toHaveBeenCalledWith(source_session);

    rerender(
      <UserMessage
        attachments={[]}
        message={{
          message_id: "report-1",
          input_id: null,
          text: "任务已完成",
          attachment_ids: [],
          quotes: [],
          source: { type: "proxy_report", source_session_id: "source-1", source_run_id: "run-2", source_goal_id: "goal-1", source_run_status: "completed" },
          created_at_ms: 2,
        }}
        on_attachment_click={vi.fn()}
        on_source_open={open}
        source_session={source_session}
      />,
    );
    expect(screen.getByText("会话报告 · 已完成")).toBeInTheDocument();
    expect(screen.getByText("关联目标")).toBeInTheDocument();

    rerender(
      <UserMessage
        attachments={[]}
        message={{
          message_id: "device-1",
          input_id: "input-1",
          text: "从终端发起的问题",
          attachment_ids: [],
          quotes: [],
          source: {
            type: "device",
            device_id: "device-1",
            device_name: "客厅终端",
            modality: "speech_transcript",
            requested_output: "audio",
          },
          created_at_ms: 3,
        }}
        on_attachment_click={vi.fn()}
        on_source_open={open}
      />,
    );
    const message = screen.getByText("从终端发起的问题");
    const source_label = screen.getByRole("button", { name: "查看消息来源：客厅终端" });
    expect(source_label).toHaveTextContent(/^客厅终端$/);
    expect(screen.queryByText("客厅终端 · 语音")).not.toBeInTheDocument();
    expect(message.compareDocumentPosition(source_label) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);

    fireEvent.click(source_label);
    const detail = screen.getByRole("dialog", { name: "消息来源详情" });
    expect(detail).toHaveTextContent("客厅终端");
    expect(within(detail).getByText("设备 ID").nextElementSibling).toHaveTextContent("device-1");
    expect(within(detail).getByText("输入方式").nextElementSibling).toHaveTextContent("语音转写");
    expect(within(detail).getByText("回复偏好").nextElementSibling).toHaveTextContent("语音");
    fireEvent.click(within(detail).getByRole("button", { name: "关闭消息来源详情" }));
    expect(screen.queryByRole("dialog", { name: "消息来源详情" })).not.toBeInTheDocument();
  });

  it("collapses only overflowing proxy reports and allows expanding them", () => {
    vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(360);

    render(
      <UserMessage
        attachments={[]}
        message={{
          message_id: "long-report",
          input_id: null,
          text: "较长的会话报告",
          attachment_ids: [],
          quotes: [],
          source: {
            type: "proxy_report",
            source_session_id: "source-1",
            source_run_id: "run-1",
            source_run_status: "completed",
          },
          created_at_ms: 1,
        }}
        on_attachment_click={vi.fn()}
        on_source_open={vi.fn()}
      />,
    );

    const expand = screen.getByRole("button", { name: "展开完整会话报告" });
    expect(expand).toHaveAttribute("aria-expanded", "false");
    expect(expand.closest("[data-expanded]")).toHaveAttribute("data-expanded", "false");

    fireEvent.click(expand);
    const collapse = screen.getByRole("button", { name: "收起" });
    expect(collapse).toHaveAttribute("aria-expanded", "true");
    expect(collapse.closest("[data-expanded]")).toHaveAttribute("data-expanded", "true");
  });

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

  it("freezes context around the selected occurrence of repeated short text", async () => {
    const fetch_spy = vi.fn();
    vi.stubGlobal("fetch", fetch_spy);
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 1,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        attachments: [],
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 2,
          items: [{
            type: "user",
            message_id: "message-repeated",
            text: "同意，然后再同意",
            attachment_ids: [],
            quotes: [],
            created_at_ms: 1,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
    const add_quote = vi.spyOn(store.composer_quotes, "add");
    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    const message = screen.getByText("同意，然后再同意");
    const text = message.firstChild;
    expect(text).not.toBeNull();
    const range = document.createRange();
    range.setStart(text!, 6);
    range.setEnd(text!, 8);
    range.getBoundingClientRect = () => new DOMRect(10, 20, 30, 12);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);

    const message_list = screen.getByLabelText("消息列表");
    const context_menu = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    expect(message_list.dispatchEvent(context_menu)).toBe(true);
    expect(context_menu.defaultPrevented).toBe(false);
    expect(screen.queryByRole("menuitem")).not.toBeInTheDocument();

    fireEvent.mouseUp(message_list);
    fireEvent.click(await screen.findByRole("button", { name: "引用" }));

    expect(add_quote).toHaveBeenCalledWith("session-1", expect.objectContaining({
      source_generation: 2,
      source_message_id: "message-repeated",
      exact: "同意",
      prefix: "同意，然后再",
      suffix: "",
    }));
    expect(fetch_spy).not.toHaveBeenCalled();
  });

  it("centers a located quote range instead of the whole message", () => {
    const store = conversationStore();
    const original_range_rect = Range.prototype.getBoundingClientRect;
    const original_scroll_into_view = Element.prototype.scrollIntoView;
    const message_scroll = vi.fn();
    let scroll_top = 100;
    Object.defineProperty(Range.prototype, "getBoundingClientRect", {
      configurable: true,
      value: () => new DOMRect(20, 600 - scroll_top, 80, 20),
    });
    Object.defineProperty(Element.prototype, "scrollIntoView", {
      configurable: true,
      value: message_scroll,
    });
    try {
      render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);
      const message_list = screen.getByLabelText("消息列表");
      const scroll_to = vi.fn((options: ScrollToOptions) => {
        scroll_top = Number(options.top ?? scroll_top);
      });
      Object.defineProperties(message_list, {
        scrollHeight: { configurable: true, get: () => 1_000 },
        clientHeight: { configurable: true, get: () => 200 },
        scrollTop: {
          configurable: true,
          get: () => scroll_top,
          set: (value: number) => { scroll_top = value; },
        },
        scrollTo: { configurable: true, value: scroll_to },
        getBoundingClientRect: {
          configurable: true,
          value: () => new DOMRect(0, 100, 600, 200),
        },
      });

      act(() => {
        store.navigation.navigateTo({
          session_id: "session-1",
          child_task_id: null,
          anchor_message_id: "message-1",
          scroll_offset: null,
        });
        store.transient_focus.focus({
          quote_id: "quote-1",
          exact: "开始",
          prefix: "",
          suffix: "",
          source_owner: { type: "main_session", session_id: "session-1" },
          source_generation: 0,
          source_message_id: "message-1",
          text_start_utf16: 0,
          text_end_utf16: 2,
          source_role: "user",
          source_label: "当前会话",
          source_available: true,
        });
      });

      expect(scroll_to).toHaveBeenCalledWith({ top: 410, behavior: "auto" });
      expect(message_scroll).not.toHaveBeenCalled();
      fireEvent.scroll(message_list);
      expect(store.transient_focus.target?.message_id).toBe("message-1");
    } finally {
      if (original_range_rect) {
        Object.defineProperty(Range.prototype, "getBoundingClientRect", {
          configurable: true,
          value: original_range_rect,
        });
      } else {
        delete (Range.prototype as Partial<Range>).getBoundingClientRect;
      }
      if (original_scroll_into_view) {
        Object.defineProperty(Element.prototype, "scrollIntoView", {
          configurable: true,
          value: original_scroll_into_view,
        });
      } else {
        delete (Element.prototype as Partial<Element>).scrollIntoView;
      }
    }
  });

  it("loads image thumbnails from the native bridge and exposes the shared preview action", async () => {
    vi.mocked(thumbnailAttachment).mockResolvedValue("data:image/jpeg;base64,dGh1bWI=");
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 0,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        attachments: [{
          attachment_id: "image-1",
          session_id: "session-1",
          original_name: "示意图.png",
          media_type: "image/png",
          size_bytes: 1024,
          agent_readable_path: "/private/runtime/image-1.png",
          state: "ready",
          created_at_ms: 1,
        }],
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 1,
          items: [{
            type: "user",
            message_id: "message-with-image",
            text: "解释图片",
            attachment_ids: ["image-1"],
            created_at_ms: 1,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(await screen.findByRole("img", { name: "示意图.png" })).toHaveAttribute(
      "src",
      "data:image/jpeg;base64,dGh1bWI=",
    );
    expect(screen.getByRole("button", { name: "预览图片 示意图.png" })).toBeEnabled();
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
        step: 1,
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
      step: 1,
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
            step: 1,
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
        step: 2,
        part_id: "reasoning-2",
        delta: "继续分析",
      },
      {
        type: "tool_proposed",
        session_id: "session-1",
        run_id: "run-1",
        step: 2,
        call_id: "call-live-1",
        tool_name: "shell",
      },
      {
        type: "tool_proposed",
        session_id: "session-1",
        run_id: "run-1",
        step: 2,
        call_id: "call-live-2",
        tool_name: "shell",
      },
      {
        type: "tool_output",
        session_id: "session-1",
        run_id: "run-1",
        step: 2,
        call_id: "call-live-2",
        channel: "stdout",
        chunk: "SMOKE OK",
      },
      {
        type: "tool_completed",
        session_id: "session-1",
        run_id: "run-1",
        step: 2,
        call_id: "call-live-1",
        status: "completed",
      },
      {
        type: "tool_completed",
        session_id: "session-1",
        run_id: "run-1",
        step: 2,
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

  it("shows one actionable failure state without leaking runtime diagnostics", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = conversationStore();
    emitLive(store, [{
      type: "model_attempt_failed",
      session_id: "session-1",
      run_id: "run-failed",
      attempt: 1,
      kind: "configuration",
      will_retry: false,
    }, {
      type: "run_finished",
      session_id: "session-1",
      run_id: "run-failed",
      status: "failed",
      error: {
        code: "model_execution_failed",
        message: "model execution failed before stream establishment",
      },
    }], () => frame);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByRole("alert")).toHaveTextContent("当前模型配置无法用于此会话，请检查配置或切换模型后重试。");
    expect(screen.queryByText("正在准备…")).not.toBeInTheDocument();
    expect(screen.queryByText(/model execution failed before stream establishment/)).not.toBeInTheDocument();
    expect(screen.queryByText("执行失败")).not.toBeInTheDocument();
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

  it("does not render a child task without its parent tool call in the loaded message list", () => {
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
            message_id: "assistant-other-run",
            run_id: "run-other",
            attempt: 1,
            created_at_ms: 1,
            finished_at_ms: 2,
            status: "completed",
            segments: [{ type: "text", part_id: "other-text", text: "当前分页内容" }],
            usage: null,
            can_fork: true,
            fork_point: "assistant-other-run",
            feedback: null,
          }],
          previous_cursor: "older-page",
          has_more: true,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByText("当前分页内容")).toBeVisible();
    expect(screen.queryByRole("button", { name: "查看子任务：检查恢复一致性" })).not.toBeInTheDocument();
  });

  it("does not attach a child task to its parent run when the exact tool call is absent", () => {
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
            message_id: "assistant-parent-without-tool",
            run_id: "run-parent",
            attempt: 1,
            created_at_ms: 1,
            finished_at_ms: 2,
            status: "completed",
            segments: [{ type: "text", part_id: "parent-text", text: "父回合没有对应工具调用" }],
            usage: null,
            can_fork: true,
            fork_point: "assistant-parent-without-tool",
            feedback: null,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByText("父回合没有对应工具调用")).toBeVisible();
    expect(screen.queryByRole("button", { name: "查看子任务：检查恢复一致性" })).not.toBeInTheDocument();
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

  it("shows and opens a live child task from its live parent tool call", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = conversationStore();

    emitLive(store, [{
      type: "run_started",
      session_id: "session-1",
      run_id: "run-live-parent",
    }, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-live-parent",
      step: 1,
      call_id: "call-delegate",
      tool_name: "delegate_task",
    }, {
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

    expect(screen.getByText("子智能体尚未产生可展示的消息。")).toBeVisible();
  });

  it("keeps compacted messages visible and expands the context summary at the boundary", () => {
    const store = conversationStore();
    store.projection.applySessionSnapshot({
      observed_sequence: 1,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 1,
          items: [{
            type: "user",
            message_id: "compacted-message",
            text: "压缩前仍可见",
            created_at_ms: 1,
          }, {
            type: "user",
            message_id: "retained-message",
            text: "压缩后保留的近期消息",
            created_at_ms: 2,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);
    store.projection.applySessionSnapshot({
      observed_sequence: 2,
      value: {
        session: { session_id: "session-1" },
        active_run: null,
        conversation: {
          owner: { type: "main_session", session_id: "session-1" },
          generation: 2,
          items: [{
            type: "user",
            message_id: "compacted-message",
            text: "压缩前仍可见",
            created_at_ms: 1,
          }, {
            type: "context_summary",
            message_id: "summary-message",
            text: "这是供模型继续工作的上下文摘要。",
          }, {
            type: "user",
            message_id: "retained-message",
            text: "压缩后保留的近期消息",
            created_at_ms: 2,
          }],
          previous_cursor: null,
          has_more: false,
        },
      },
    } as unknown as Parameters<RootStore["projection"]["applySessionSnapshot"]>[0]);

    render(<RootStoreProvider store={store}><ConversationView /></RootStoreProvider>);

    expect(screen.getByText("压缩前仍可见")).toBeVisible();
    expect(screen.getByText("压缩后保留的近期消息")).toBeVisible();
    const toggle = screen.getByRole("button", { name: "查看上下文摘要" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("这是供模型继续工作的上下文摘要。")).not.toBeInTheDocument();

    fireEvent.click(toggle);
    expect(screen.getByText("这是供模型继续工作的上下文摘要。")).toBeVisible();
    expect(screen.getByRole("button", { name: "收起上下文摘要" })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
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
