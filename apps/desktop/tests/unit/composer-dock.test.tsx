import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
    ));
    await waitFor(() => expect(input).toHaveValue(""));

    await user.type(input, "/model");
    fireEvent.keyDown(input, { key: "Enter" });
    await user.click(await screen.findByRole("menuitemradio", { name: /备用模型/ }));
    await waitFor(() => expect(set_model).toHaveBeenCalledWith("session-1", "alternate"));
    await waitFor(() => expect(screen.queryByRole("menuitemradio", { name: /备用模型/ })).not.toBeInTheDocument());

    await user.type(input, "/mode");
    fireEvent.keyDown(input, { key: "Enter" });
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
        { input_id: "input-1", text_preview: "先检查构建", submitted_at_ms: 1, position: 1, is_prioritized: false, held_by_goal: false, source: { type: "user" } },
        { input_id: "input-2", text_preview: "再运行测试", submitted_at_ms: 2, position: 2, is_prioritized: false, held_by_goal: false, source: { type: "user" } },
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
      "session-1", "准备当前版本", "build", [], "normal", "release",
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
      items: [{
        input_id: "queued-during-approval",
        text_preview: "审批后执行下一项",
        submitted_at_ms: 2,
        position: 1,
        is_prioritized: false,
        held_by_goal: false,
        source: { type: "user" },
      }],
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
    ));
    expect(screen.queryByText("notes.md")).not.toBeInTheDocument();
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
    const confirm_exit = vi.spyOn(window, "confirm")
      .mockReturnValueOnce(false)
      .mockReturnValue(true);

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
    expect(confirm_exit).toHaveBeenCalledTimes(1);
    expect(clear_goal).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "退出目标" }));
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
      items: [{
        input_id: "input-held",
        text_preview: "补充检查边界情况",
        submitted_at_ms: 1,
        position: 1,
        is_prioritized: false,
        held_by_goal: true,
        source: { type: "user" },
      }],
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
      user_directory: "/workspace/project",
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
      context: null,
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
