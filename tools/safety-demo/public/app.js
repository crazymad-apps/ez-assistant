(() => {
  "use strict";

  const MAX_ACTIVITY_ITEMS = 200;
  const MAX_RECOVERY_ATTEMPTS = 3;
  const STREAM_RENDER_INTERVAL_MS = 40;
  const CONVERSATION_BOTTOM_TOLERANCE_PX = 48;

  const elements = {
    loadingView: document.querySelector("#loading-view"),
    blockedView: document.querySelector("#blocked-view"),
    blockedTitle: document.querySelector("#blocked-title"),
    blockedMessage: document.querySelector("#blocked-message"),
    app: document.querySelector("#app"),
    connectionBanner: document.querySelector("#connection-banner"),
    connectionBadge: document.querySelector("#connection-badge"),
    riskBadge: document.querySelector("#risk-badge"),
    sessionWorkdir: document.querySelector("#session-workdir"),
    temporaryWorkspace: document.querySelector("#temporary-workspace"),
    sessionId: document.querySelector("#session-id"),
    runStatus: document.querySelector("#run-status"),
    runActionButton: document.querySelector("#run-action-button"),
    runActionError: document.querySelector("#run-action-error"),
    conversation: document.querySelector("#conversation"),
    composer: document.querySelector("#composer"),
    draftRiskBadge: document.querySelector("#draft-risk-badge"),
    executionModeHelp: document.querySelector("#execution-mode-help"),
    approvalModeHelp: document.querySelector("#approval-mode-help"),
    modeFreezeNote: document.querySelector("#mode-freeze-note"),
    planAutoNote: document.querySelector("#plan-auto-note"),
    buildAutoConfirmation: document.querySelector("#build-auto-confirmation"),
    messageInput: document.querySelector("#message-input"),
    composerError: document.querySelector("#composer-error"),
    sendButton: document.querySelector("#send-button"),
    sequence: document.querySelector("#event-sequence"),
    inspectorTabs: document.querySelector("#inspector-tabs"),
    approvalCount: document.querySelector("#approval-count"),
    activityCount: document.querySelector("#activity-count"),
    auditCount: document.querySelector("#audit-count"),
    approvalPanel: document.querySelector("#panel-approval"),
    activityPanel: document.querySelector("#panel-activity"),
    auditPanel: document.querySelector("#panel-audit"),
    resetDialog: document.querySelector("#reset-dialog"),
    resetWorkspacePath: document.querySelector("#reset-workspace-path"),
    resetError: document.querySelector("#reset-error"),
    resetCancelButton: document.querySelector("#reset-cancel-button"),
    resetConfirmButton: document.querySelector("#reset-confirm-button"),
    toast: document.querySelector("#toast"),
  };

  const state = createInitialViewState();
  let eventSource = null;
  let refreshTimer = null;
  let streamRenderTimer = null;
  let streamRenderFrame = null;
  let recoveryGeneration = 0;
  let toastTimer = null;
  let renderSequence = 0;

  // 页面只保存服务端投影和短生命周期草稿；Run、审批和审计仍以 snapshot 为权威。
  function createInitialViewState() {
    return {
      connection: {
        status: "loading",
        lastSeq: 0,
        recoveryAttempts: 0,
      },
      snapshot: null,
      draft: {
        message: "",
        executionMode: "plan",
        approvalMode: "ask",
        buildAutoConfirmed: false,
        buildAutoPending: false,
      },
      view: {
        inspectorTab: "audit",
        auditFilter: "all",
        activity: [],
        operation: null,
        operationError: null,
        resetOpen: false,
        resetError: null,
        previousApprovalId: null,
        focusApproval: false,
      },
      stream: {
        text: "",
        reasoning: "",
        pendingText: [],
        pendingReasoning: [],
        tools: new Map(),
      },
    };
  }

  function isObject(value) {
    return value !== null && typeof value === "object" && !Array.isArray(value);
  }

  function isSnapshot(value) {
    return (
      isObject(value) &&
      typeof value.session_id === "string" &&
      typeof value.session_workdir === "string" &&
      typeof value.temporary_workspace === "string" &&
      typeof value.active_run === "boolean" &&
      typeof value.pending_approval === "boolean" &&
      Array.isArray(value.journal) &&
      Array.isArray(value.audit) &&
      Number.isSafeInteger(value.sequence)
    );
  }

  function createElement(tag, className, text) {
    const element = document.createElement(tag);
    if (className) {
      element.className = className;
    }
    if (text !== undefined) {
      element.textContent = String(text);
    }
    return element;
  }

  function setVisible(element, isVisible) {
    element.hidden = !isVisible;
  }

  function setError(element, message) {
    element.textContent = message || "";
    setVisible(element, Boolean(message));
  }

  function applySnapshot(snapshot) {
    if (!isSnapshot(snapshot)) {
      throw new Error("snapshot_shape_invalid");
    }
    // SSE 可能在并发 snapshot 请求返回前先推进；旧 snapshot 不能覆盖较新的本地投影。
    if (state.snapshot && snapshot.sequence < state.connection.lastSeq) {
      return false;
    }
    const previousApprovalId = state.snapshot?.approval?.approval_id ?? null;
    const nextApprovalId = snapshot.approval?.approval_id ?? null;
    state.snapshot = snapshot;
    state.connection.lastSeq = snapshot.sequence;
    if (!snapshot.active_run) {
      clearStreamingProjection();
    }
    if (nextApprovalId && nextApprovalId !== previousApprovalId) {
      state.view.inspectorTab = "approval";
      state.view.focusApproval = true;
    } else if (!nextApprovalId && state.view.inspectorTab === "approval" && !snapshot.active_run) {
      state.view.inspectorTab = "audit";
    }
    state.view.previousApprovalId = nextApprovalId;
    return true;
  }

  // SSE 是带序号的失效/实时通知。出现重复事件时忽略，出现 gap 时交给恢复流程。
  function applyEvent(notification) {
    if (!isObject(notification) || !Number.isSafeInteger(notification.sequence) || !isObject(notification.kind)) {
      throw new Error("event_shape_invalid");
    }
    if (notification.sequence <= state.connection.lastSeq) {
      return false;
    }
    if (notification.sequence !== state.connection.lastSeq + 1) {
      throw new Error("event_sequence_gap");
    }
    state.connection.lastSeq = notification.sequence;
    const kind = notification.kind;
    const type = typeof kind.type === "string" ? kind.type : "unknown";

    if (type === "run_started") {
      clearStreamingProjection();
      state.view.activity = [];
    }
    if (type === "run_progress" && isObject(kind.detail)) {
      applyProgressDetail(kind.detail);
    }
    if (type === "approval_changed" && kind.approval_id) {
      state.view.inspectorTab = "approval";
    }
    appendActivity(notification);
    return true;
  }

  function applyProgressDetail(detail) {
    switch (detail.type) {
      case "step_started":
        state.stream.text = "";
        state.stream.reasoning = "";
        break;
      case "text_delta":
        if (typeof detail.delta === "string") {
          state.stream.pendingText.push(detail.delta);
        }
        break;
      case "reasoning_delta":
        if (typeof detail.delta === "string") {
          state.stream.pendingReasoning.push(detail.delta);
        }
        break;
      case "tool_proposed":
        if (typeof detail.call_id === "string") {
          const current = streamTool(detail.call_id);
          current.toolName = stringOr(detail.tool_name, current.toolName || "unknown");
          current.status = "resolved";
        }
        break;
      case "tool_started":
        if (typeof detail.call_id === "string") {
          streamTool(detail.call_id).status = "running";
        }
        break;
      case "tool_output":
        if (typeof detail.call_id === "string" && typeof detail.chunk === "string") {
          const current = streamTool(detail.call_id);
          const channel = detail.channel === "stderr" ? "stderr" : "stdout";
          current[channel === "stderr" ? "pendingStderr" : "pendingStdout"].push(detail.chunk);
        }
        break;
      case "tool_completed":
        if (typeof detail.call_id === "string") {
          streamTool(detail.call_id).status = detail.status === "success" ? "success" : "failed";
        }
        break;
      default:
        break;
    }
  }

  function streamTool(callId) {
    let tool = state.stream.tools.get(callId);
    if (!tool) {
      tool = {
        callId,
        toolName: "unknown",
        status: "resolved",
        stdout: "",
        stderr: "",
        pendingStdout: [],
        pendingStderr: [],
      };
      state.stream.tools.set(callId, tool);
    }
    return tool;
  }

  function clearStreamingProjection() {
    state.stream.text = "";
    state.stream.reasoning = "";
    state.stream.pendingText = [];
    state.stream.pendingReasoning = [];
    state.stream.tools.clear();
  }

  // 高频模型分片先在内存中合并，再按固定刷新周期更新局部 DOM。
  // 这样既保留完整流式内容，也避免每个 token 都复制累计文本并重建整个页面。
  function flushPendingStreamData() {
    const textDelta = state.stream.pendingText.join("");
    const reasoningDelta = state.stream.pendingReasoning.join("");
    const toolDeltas = [];
    state.stream.pendingText = [];
    state.stream.pendingReasoning = [];
    if (textDelta) {
      state.stream.text += textDelta;
    }
    if (reasoningDelta) {
      state.stream.reasoning += reasoningDelta;
    }
    for (const tool of state.stream.tools.values()) {
      const stdout = tool.pendingStdout.join("");
      const stderr = tool.pendingStderr.join("");
      tool.pendingStdout = [];
      tool.pendingStderr = [];
      if (stdout) {
        tool.stdout += stdout;
        toolDeltas.push({ callId: tool.callId, channel: "stdout", delta: stdout });
      }
      if (stderr) {
        tool.stderr += stderr;
        toolDeltas.push({ callId: tool.callId, channel: "stderr", delta: stderr });
      }
    }
    return { textDelta, reasoningDelta, toolDeltas };
  }

  function appendActivity(notification) {
    const kind = notification.kind;
    const progressType = kind.type === "run_progress" ? kind.detail?.type : null;
    const aggregationKey = activityAggregationKey(kind);
    const previous = state.view.activity[state.view.activity.length - 1];
    if (aggregationKey && previous?.aggregationKey === aggregationKey) {
      const detail = safeActivityDetail(kind);
      previous.sequence = notification.sequence;
      previous.timestamp = Date.now();
      previous.occurrences += 1;
      const sizeField = progressType === "tool_output" ? "chunk_characters" : "delta_characters";
      previous.detail.detail[sizeField] =
        numberOr(previous.detail.detail[sizeField], 0) + numberOr(detail.detail[sizeField], 0);
      return;
    }
    const activity = {
      sequence: notification.sequence,
      timestamp: Date.now(),
      title: activityTitle(kind),
      detail: safeActivityDetail(kind),
      tone: activityTone(kind),
      progressType,
      aggregationKey,
      occurrences: 1,
    };
    state.view.activity.push(activity);
    if (state.view.activity.length > MAX_ACTIVITY_ITEMS) {
      state.view.activity.splice(0, state.view.activity.length - MAX_ACTIVITY_ITEMS);
    }
  }

  function activityAggregationKey(kind) {
    const detail = kind.type === "run_progress" && isObject(kind.detail) ? kind.detail : null;
    if (detail?.type === "text_delta" || detail?.type === "reasoning_delta") {
      return detail.type;
    }
    if (detail?.type === "tool_output") {
      return `${detail.type}:${stringOr(detail.call_id, "unknown")}:${stringOr(detail.channel, "stdout")}`;
    }
    return null;
  }

  function safeActivityDetail(kind) {
    if (kind.type !== "run_progress" || !isObject(kind.detail)) {
      return kind;
    }
    const detail = { ...kind.detail };
    if (typeof detail.delta === "string") {
      detail.delta_characters = Array.from(detail.delta).length;
      delete detail.delta;
    }
    if (typeof detail.chunk === "string") {
      detail.chunk_characters = Array.from(detail.chunk).length;
      delete detail.chunk;
    }
    return { ...kind, detail };
  }

  function activityTitle(kind) {
    if (!isObject(kind)) {
      return "未知事件";
    }
    switch (kind.type) {
      case "session_reset":
        return "Session 已重置";
      case "run_started":
        return `Run started · ${stringOr(kind.run_id, "unknown")}`;
      case "run_finished":
        return `Run finished · ${runStatusLabel(kind.status)}`;
      case "approval_changed":
        return kind.approval_id ? "出现待审批动作" : "审批状态已更新";
      case "run_progress":
        return progressTitle(kind.event, kind.detail);
      default:
        return "未知事件";
    }
  }

  function progressTitle(event, detail) {
    if (isObject(detail)) {
      switch (detail.type) {
        case "step_started":
          return `模型 Step ${detail.step} 开始`;
        case "text_delta":
          return "Assistant 正在生成正文";
        case "reasoning_delta":
          return "模型正在生成 Reasoning";
        case "tool_proposed":
          return `${stringOr(detail.tool_name, "工具")} 参数已解析`;
        case "tool_started":
          return `${stringOr(detail.call_id, "工具")} 开始执行`;
        case "tool_output":
          return `${stringOr(detail.call_id, "Shell")} 输出 · ${stringOr(detail.channel, "stdout")}`;
        case "tool_completed":
          return `${stringOr(detail.call_id, "工具")} 完成 · ${stringOr(detail.status, "unknown")}`;
        case "guardrail_triggered":
          return `Guardrail 触发 · ${guardrailKindLabel(detail.kind)}`;
        case "usage_updated":
          return `Token 用量已确认 · ${numberOr(detail.total_tokens, 0)}`;
        default:
          break;
      }
    }
    const labels = {
      execution_started: "AgentExecution 已启动",
      cancel_requested: "已请求停止 Run",
      execution_completed: "Core 执行完成",
      execution_failed: "Core 执行失败",
      execution_cancelled: "Core 执行已取消",
      execution_compaction_required: "Core 请求上下文压缩",
    };
    return labels[event] || stringOr(event, "Run progress");
  }

  function activityTone(kind) {
    const detail = isObject(kind.detail) ? kind.detail : null;
    if (kind.type === "run_finished" && kind.status === "failed") {
      return "failed";
    }
    if (detail?.type === "guardrail_triggered") {
      return detail.mode === "enforce" ? "enforce" : "waiting";
    }
    if (detail?.type === "tool_completed" && detail.status === "failed") {
      return "failed";
    }
    return "neutral";
  }

  function render() {
    cancelScheduledStreamRender();
    flushPendingStreamData();
    renderSequence += 1;
    const hasSnapshot = Boolean(state.snapshot);
    setVisible(elements.loadingView, state.connection.status === "loading" && !hasSnapshot);
    setVisible(elements.blockedView, state.connection.status === "disconnected" && !hasSnapshot);
    setVisible(elements.app, hasSnapshot);
    if (!hasSnapshot) {
      return;
    }
    renderConnection();
    renderHeader();
    renderConversation();
    renderInspector();
    renderComposer();
    renderResetDialog();
    focusPendingApproval();
  }

  function renderConnection() {
    const status = state.connection.status;
    const labels = {
      loading: "正在连接",
      connected: "已连接",
      recovering: "正在恢复",
      disconnected: "已断开",
    };
    elements.connectionBadge.textContent = labels[status] || "未知";
    elements.connectionBadge.className = "badge";
    elements.connectionBadge.classList.add(connectionBadgeClass(status));
    if (status === "recovering") {
      elements.connectionBanner.textContent =
        "连接中断，正在从权威 snapshot 恢复。后端 Run 可能仍在继续，当前写操作已禁用。";
      setVisible(elements.connectionBanner, true);
    } else if (status === "disconnected") {
      elements.connectionBanner.textContent =
        "Safety Demo 连接已断开。已有内容保持只读；若服务已重启，请打开终端中新的地址。";
      setVisible(elements.connectionBanner, true);
    } else {
      setVisible(elements.connectionBanner, false);
    }
  }

  function renderHeader() {
    const snapshot = state.snapshot;
    elements.sessionWorkdir.textContent = snapshot.session_workdir;
    elements.sessionWorkdir.title = snapshot.session_workdir;
    elements.temporaryWorkspace.textContent = snapshot.temporary_workspace;
    elements.temporaryWorkspace.title = snapshot.temporary_workspace;
    elements.sessionId.textContent = snapshot.session_id;
    elements.sequence.textContent = `seq ${state.connection.lastSeq}`;

    const run = snapshot.run;
    const isRisk = Boolean(
      (run && run.execution_mode === "build" && run.approval_mode === "auto") ||
        (state.draft.buildAutoConfirmed &&
          state.draft.executionMode === "build" &&
          state.draft.approvalMode === "auto"),
    );
    setVisible(elements.riskBadge, isRisk);
    elements.runStatus.textContent = runHeaderLabel(snapshot);
    setError(elements.runActionError, state.view.operationError?.area === "run" ? state.view.operationError.message : null);

    const connected = state.connection.status === "connected";
    const isCancelling = state.view.operation === "cancel" || Boolean(run?.cancel_requested);
    if (snapshot.active_run) {
      let actionLabel = "停止运行";
      if (!connected) {
        actionLabel = "连接中断，状态未知";
      } else if (isCancelling) {
        actionLabel = "正在停止…";
      }
      elements.runActionButton.textContent = actionLabel;
      elements.runActionButton.dataset.action = "cancel-run";
      elements.runActionButton.className = "button button-danger";
      elements.runActionButton.disabled = !connected || isCancelling;
    } else {
      elements.runActionButton.textContent = "重置 Session";
      elements.runActionButton.dataset.action = "open-reset";
      elements.runActionButton.className = "button button-secondary";
      elements.runActionButton.disabled =
        !connected || snapshot.pending_approval || snapshot.is_resetting || state.view.operation === "reset";
    }
  }

  function runHeaderLabel(snapshot) {
    const run = snapshot.run;
    if (!run) {
      return "空闲";
    }
    const mode = `${modeLabel(run.execution_mode)} + ${modeLabel(run.approval_mode)}`;
    if (snapshot.pending_approval) {
      return `${run.run_id} · ${mode} · 等待审批`;
    }
    if (run.cancel_requested && run.status === "running") {
      return `${run.run_id} · ${mode} · 正在停止`;
    }
    if (run.status === "running") {
      const isToolRunning = Array.from(state.stream.tools.values()).some(
        (tool) => tool.status === "running",
      );
      if (isToolRunning) {
        return `${run.run_id} · ${mode} · 工具执行中`;
      }
      if (state.stream.text || state.stream.reasoning) {
        return `${run.run_id} · ${mode} · 模型生成中`;
      }
    }
    return `${run.run_id} · ${mode} · ${runStatusLabel(run.status)}`;
  }

  function renderConversation() {
    const shouldFollowBottom = isConversationNearBottom();
    const snapshot = state.snapshot;
    const fragment = document.createDocumentFragment();
    const messages = Array.isArray(snapshot.journal) ? snapshot.journal : [];
    const resultByCall = new Map();
    const renderedCalls = new Set();
    for (const message of messages) {
      if (message?.role === "tool" && isObject(message.turn?.result)) {
        resultByCall.set(message.turn.result.call_id, message.turn.result);
      }
    }
    const auditByCall = new Map(snapshot.audit.map((entry) => [entry.call_id, entry]));

    for (const message of messages) {
      if (!isObject(message) || !isObject(message.turn)) {
        continue;
      }
      if (message.role === "user") {
        fragment.append(renderUserMessage(message.turn));
      } else if (message.role === "assistant") {
        fragment.append(renderAssistantMessage(message.turn));
        for (const part of arrayOr(message.turn.parts, [])) {
          if (part?.type !== "tool_call" || !isObject(part.data)) {
            continue;
          }
          renderedCalls.add(part.data.id);
          fragment.append(
            renderToolCard(part.data, resultByCall.get(part.data.id), auditByCall.get(part.data.id)),
          );
        }
      }
    }

    if (snapshot.active_run && (state.stream.text || state.stream.reasoning)) {
      fragment.append(renderStreamingAssistant());
    }
    for (const tool of state.stream.tools.values()) {
      if (!renderedCalls.has(tool.callId)) {
        fragment.append(renderLiveToolCard(tool, auditByCall.get(tool.callId)));
      }
    }
    if (snapshot.run && snapshot.run.status !== "running") {
      fragment.append(renderTerminal(snapshot.run));
    }
    if (!fragment.childNodes.length) {
      fragment.append(
        emptyState(
          "还没有对话",
          "在下方选择下一次运行的两个模式，然后提交一个任务。Plan + Ask 是默认安全起点。",
        ),
      );
    }
    elements.conversation.replaceChildren(fragment);
    if (shouldFollowBottom) {
      scrollConversationToBottom();
    }
  }

  function renderUserMessage(message) {
    const card = createElement("article", "message-card message-user");
    const meta = createElement("div", "message-meta");
    meta.append(createElement("strong", "", "你"), createElement("span", "", stringOr(message.id, "User")));
    card.append(meta);
    const texts = arrayOr(message.parts, [])
      .filter((part) => part?.type === "text" && isObject(part.data))
      .map((part) => stringOr(part.data.text, ""));
    card.append(createElement("p", "message-text", texts.join("\n")));
    return card;
  }

  function renderAssistantMessage(message) {
    const card = createElement("article", "message-card message-assistant");
    const meta = createElement("div", "message-meta");
    const model = isObject(message.model)
      ? `${stringOr(message.model.provider, "model")} / ${stringOr(message.model.model, "unknown")}`
      : "Assistant";
    meta.append(createElement("strong", "", "Assistant"), createElement("span", "", model));
    card.append(meta);
    const text = [];
    const reasoning = [];
    for (const part of arrayOr(message.parts, [])) {
      if (part?.type === "text" && isObject(part.data)) {
        text.push(stringOr(part.data.text, ""));
      } else if (part?.type === "reasoning" && isObject(part.data)) {
        reasoning.push(stringOr(part.data.text, ""));
      }
    }
    if (text.length) {
      card.append(createElement("p", "message-text", text.join("")));
    } else if (arrayOr(message.parts, []).some((part) => part?.type === "tool_call")) {
      card.append(createElement("p", "muted", "Assistant 请求执行以下工具调用。"));
    }
    if (reasoning.length) {
      card.append(detailsBlock("Reasoning（调试）", reasoning.join(""), "code-block"));
    }
    const finish = finishReasonLabel(message.finish_reason);
    const usage = isObject(message.usage) ? ` · ${numberOr(message.usage.total_tokens, 0)} tokens` : "";
    card.append(createElement("p", "meta-line", `${finish}${usage}`));
    return card;
  }

  function renderStreamingAssistant() {
    const card = createElement("article", "message-card message-assistant");
    card.dataset.streamingAssistant = "true";
    const meta = createElement("div", "message-meta");
    meta.append(createElement("strong", "", "Assistant"), createElement("span", "status-running", "生成中…"));
    card.append(meta);
    if (state.stream.text) {
      const text = createElement("p", "message-text", state.stream.text);
      text.dataset.streamingText = "true";
      card.append(text);
    }
    if (state.stream.reasoning) {
      card.append(streamingReasoningBlock(state.stream.reasoning));
    }
    return card;
  }

  function streamingReasoningBlock(text) {
    const details = detailsBlock("Reasoning（调试）", text, "code-block");
    details.dataset.streamingReasoning = "true";
    details.querySelector("pre").dataset.streamingReasoningText = "true";
    return details;
  }

  function isConversationNearBottom() {
    const remaining =
      elements.conversation.scrollHeight -
      elements.conversation.scrollTop -
      elements.conversation.clientHeight;
    return remaining <= CONVERSATION_BOTTOM_TOLERANCE_PX;
  }

  function scrollConversationToBottom() {
    elements.conversation.scrollTop = elements.conversation.scrollHeight;
  }

  function scheduleStreamRender() {
    if (streamRenderTimer !== null || streamRenderFrame !== null) {
      return;
    }
    streamRenderTimer = window.setTimeout(() => {
      streamRenderTimer = null;
      streamRenderFrame = window.requestAnimationFrame(() => {
        streamRenderFrame = null;
        renderStreamUpdate();
      });
    }, STREAM_RENDER_INTERVAL_MS);
  }

  function cancelScheduledStreamRender() {
    if (streamRenderTimer !== null) {
      window.clearTimeout(streamRenderTimer);
      streamRenderTimer = null;
    }
    if (streamRenderFrame !== null) {
      window.cancelAnimationFrame(streamRenderFrame);
      streamRenderFrame = null;
    }
  }

  function renderStreamUpdate() {
    const { textDelta, reasoningDelta, toolDeltas } = flushPendingStreamData();
    if (!state.snapshot) {
      return;
    }
    const shouldFollowBottom = isConversationNearBottom();
    let card = elements.conversation.querySelector("[data-streaming-assistant=true]");
    if (!card && (state.stream.text || state.stream.reasoning)) {
      card = renderStreamingAssistant();
      elements.conversation.append(card);
    } else if (card) {
      appendStreamingText(card, textDelta, reasoningDelta);
    }
    const isToolStructureMissing = toolDeltas.some(
      (toolDelta) => !findToolCard(toolDelta.callId),
    );
    if (isToolStructureMissing) {
      renderConversation();
    } else {
      for (const toolDelta of toolDeltas) {
        appendStreamingToolOutput(toolDelta);
      }
    }
    renderHeader();
    elements.sequence.textContent = `seq ${state.connection.lastSeq}`;
    if (state.view.inspectorTab === "activity") {
      renderActivity();
    }
    if (shouldFollowBottom) {
      scrollConversationToBottom();
    }
  }

  function appendStreamingText(card, textDelta, reasoningDelta) {
    if (textDelta) {
      let text = card.querySelector("[data-streaming-text=true]");
      if (text) {
        text.append(document.createTextNode(textDelta));
      } else {
        text = createElement("p", "message-text", state.stream.text);
        text.dataset.streamingText = "true";
        const reasoning = card.querySelector("[data-streaming-reasoning=true]");
        card.insertBefore(text, reasoning);
      }
    }
    if (reasoningDelta) {
      const reasoning = card.querySelector("[data-streaming-reasoning=true]");
      if (reasoning) {
        reasoning
          .querySelector("[data-streaming-reasoning-text=true]")
          .append(document.createTextNode(reasoningDelta));
      } else {
        card.append(streamingReasoningBlock(state.stream.reasoning));
      }
    }
  }

  function findToolCard(callId) {
    return Array.from(elements.conversation.querySelectorAll("[data-tool-call-id]")).find(
      (candidate) => candidate.dataset.toolCallId === callId,
    );
  }

  function appendStreamingToolOutput(toolDelta) {
    const card = findToolCard(toolDelta.callId);
    let section = card.querySelector("[data-live-output=true]");
    if (!section) {
      section = createElement("section", "tool-result");
      section.dataset.liveOutput = "true";
      section.append(createElement("strong", "", "实时输出"));
      card.append(section);
    }
    let block = section.querySelector(`[data-channel=${toolDelta.channel}]`);
    if (!block) {
      block = createElement("pre", "stream-block");
      block.dataset.channel = toolDelta.channel;
      block.tabIndex = 0;
      section.append(block);
    }
    block.append(document.createTextNode(toolDelta.delta));
    block.scrollTop = block.scrollHeight;
  }

  function renderToolCard(call, result, audit) {
    const callId = stringOr(call.id, "unknown-call");
    const stream = state.stream.tools.get(callId);
    const status = toolDisplayStatus(result, audit, stream);
    const card = createElement("article", "tool-card");
    card.dataset.toolCallId = callId;
    card.dataset.status = status;
    card.append(toolHeading(stringOr(call.name, "unknown"), audit, status));
    appendFacts(card, audit?.facts, callId);
    if (audit) {
      card.append(createElement("p", "meta-line", `策略：${audit.policy} · ${audit.rule}`));
    }
    if (stream && (stream.stdout || stream.stderr)) {
      card.append(renderLiveOutput(stream));
    }
    if (result) {
      card.append(renderToolResult(result, audit));
    }
    return card;
  }

  function renderLiveToolCard(tool, audit) {
    const card = createElement("article", "tool-card");
    card.dataset.toolCallId = tool.callId;
    const status = audit?.status || tool.status;
    card.dataset.status = status;
    card.append(toolHeading(tool.toolName, audit, status));
    appendFacts(card, audit?.facts, tool.callId);
    if (tool.stdout || tool.stderr) {
      card.append(renderLiveOutput(tool));
    }
    return card;
  }

  function toolHeading(toolName, audit, status) {
    const heading = createElement("div", "tool-heading");
    const title = createElement("div", "tool-title");
    title.append(
      createElement("span", "kind-badge", toolKindLabel(audit?.facts?.type)),
      createElement("h3", "", toolName),
    );
    heading.append(title, createElement("span", `status-badge ${statusClass(status)}`, toolStatusLabel(status)));
    return heading;
  }

  function appendFacts(container, facts, callId) {
    const wrapper = createElement("div", "tool-facts");
    wrapper.append(factRow("Call ID", callId, true));
    if (facts?.type === "file") {
      wrapper.append(factRow("操作", stringOr(facts.operation, "unknown")));
      wrapper.append(factRow("逻辑路径", stringOr(facts.path, ""), true));
    } else if (facts?.type === "shell") {
      wrapper.append(factRow("工作目录", stringOr(facts.workdir, ""), true));
      wrapper.append(factRow("超时", `${numberOr(facts.timeout_ms, 0)} ms`));
      wrapper.append(factRow("进程模式", shellProcessModeLabel(facts.process_mode)));
      const commandId = nextId("tool-command");
      wrapper.append(factCodeRow("完整命令", stringOr(facts.command, ""), commandId));
    } else if (facts?.type === "general") {
      wrapper.append(factRow("工具", stringOr(facts.tool_name, "unknown")));
    }
    container.append(wrapper);
  }

  function shellProcessModeLabel(mode) {
    return mode === "detached" ? "Detached（脱管）" : "Managed（受管）";
  }

  function searchTruncationLabel(reason) {
    const labels = {
      max_results: "达到结果数量上限",
      max_output_bytes: "达到搜索输出总量上限",
      oversized_record: "单条搜索记录超过上限",
    };
    return labels[reason] || stringOr(reason, "未知截断原因");
  }

  function renderLiveOutput(tool) {
    const section = createElement("section", "tool-result");
    section.dataset.liveOutput = "true";
    section.append(createElement("strong", "", "实时输出"));
    if (tool.stdout) {
      const block = createElement("pre", "stream-block", tool.stdout);
      block.dataset.channel = "stdout";
      block.tabIndex = 0;
      section.append(block);
    }
    if (tool.stderr) {
      const block = createElement("pre", "stream-block", tool.stderr);
      block.dataset.channel = "stderr";
      block.tabIndex = 0;
      section.append(block);
    }
    return section;
  }

  function renderToolResult(result, audit) {
    const section = createElement("section", "tool-result");
    const content = result.content;
    if (result.status === "error") {
      const errorClass = audit?.error_class;
      section.append(createElement("strong", "status-failed", errorTitle(errorClass)));
      section.append(createElement("pre", "code-block", toolContentText(content)));
      return section;
    }
    if (content?.type === "json" && isObject(content.value)) {
      appendStructuredResult(section, content.value, audit?.facts?.type);
    } else {
      section.append(createElement("pre", "code-block", toolContentText(content)));
    }
    return section;
  }

  function appendStructuredResult(section, value, factType) {
    if (factType === "shell" || ("stdout" in value && "stderr" in value)) {
      const summary = createElement("p", "meta-line", `exit code：${value.exit_code ?? "无"} · ${shellProcessModeLabel(value.process_mode)}${value.truncated ? " · 输出已截断" : ""}`);
      section.append(summary);
      if (value.process_mode === "detached") {
        section.append(createElement("p", "approval-warning", "启动命令已完成，后台进程已脱管；这不代表服务健康，后续 Run 取消、Session reset 或 Demo 退出都不会停止它。"));
      }
      if (value.truncated) {
        section.append(createElement("p", "mode-note", "输出已达到 1 MiB 上限，后续内容已排空但未保留。"));
      }
      const grid = createElement("div", "result-grid");
      const gridId = nextId("shell-output");
      grid.id = gridId;
      grid.append(outputBlock("stdout", stringOr(value.stdout, "")), outputBlock("stderr", stringOr(value.stderr, "")));
      const expand = createElement("button", "button button-quiet", "展开到页面");
      expand.type = "button";
      expand.dataset.action = "toggle-output";
      expand.dataset.targetId = gridId;
      section.append(expand, grid);
      return;
    }
    if (typeof value.content === "string" && "offset" in value) {
      section.append(
        createElement(
          "p",
          "meta-line",
          `offset ${value.offset} · limit ${value.limit} · next ${value.next_offset ?? "无"}${value.truncated ? " · truncated" : ""}`,
        ),
        createElement("pre", "code-block", value.content),
      );
      return;
    }
    if (Array.isArray(value.entries)) {
      const list = createElement("div", "tool-result");
      for (const entry of value.entries) {
        const kind = `${stringOr(entry?.kind, "other")}${entry?.is_symlink ? " · 符号链接" : ""}`;
        list.append(factRow(kind, stringOr(entry?.path, ""), true));
      }
      section.append(list);
      return;
    }
    if (Array.isArray(value.matches)) {
      const reason = value.truncation_reason ? ` · ${searchTruncationLabel(value.truncation_reason)}` : "";
      section.append(createElement("p", "meta-line", `${value.matches.length} 个结果${value.truncated ? ` · truncated${reason}` : ""}`));
      const list = createElement("div", "tool-result");
      for (const match of value.matches) {
        const suffix = match?.type === "content" ? `:${match.line_number} ${stringOr(match.line, "")}` : "";
        list.append(createElement("code", "fact-value", `${stringOr(match?.path, "")}${suffix}`));
      }
      section.append(list);
      return;
    }
    const summaries = [];
    for (const key of ["path", "bytes_written", "replacements", "deleted", "truncated"]) {
      if (key in value) {
        summaries.push(`${key}: ${String(value[key])}`);
      }
    }
    if (summaries.length) {
      section.append(createElement("pre", "code-block", summaries.join("\n")));
    } else {
      section.append(createElement("pre", "json-block", JSON.stringify(value, null, 2)));
    }
  }

  function outputBlock(channel, text) {
    const wrapper = createElement("section", "");
    const heading = createElement("div", "tool-heading");
    heading.append(createElement("strong", "", channel));
    const block = createElement("pre", "stream-block", text || "（无输出）");
    const blockId = nextId(`shell-${channel}`);
    block.id = blockId;
    block.dataset.channel = channel;
    block.tabIndex = 0;
    const copy = createElement("button", "button button-quiet", "复制");
    copy.type = "button";
    copy.dataset.action = "copy";
    copy.dataset.copyId = blockId;
    heading.append(copy);
    wrapper.append(heading, block);
    return wrapper;
  }

  function renderTerminal(run) {
    const isEnforce = run.status === "failed" && run.last_guardrail?.mode === "enforce";
    const tone = isEnforce ? "enforce" : run.status;
    const card = createElement("section", `terminal-card terminal-${tone}`);
    card.setAttribute("role", isEnforce || run.status === "failed" ? "alert" : "status");
    card.append(createElement("strong", "", isEnforce ? "Guardrail 已终止 Run" : `Run ${runStatusLabel(run.status)}`));
    if (run.last_error) {
      card.append(createElement("p", "message-text", run.last_error));
    } else if (run.status === "cancelled") {
      card.append(createElement("p", "meta-line", "未结算的 Tool Call 已补记取消结果，工具进程清理完成。"));
    } else if (run.status === "completed") {
      card.append(createElement("p", "meta-line", "可开始下一次 Run。"));
    }
    if (run.last_guardrail) {
      card.append(createElement("p", "meta-line", guardrailSummary(run.last_guardrail)));
    }
    return card;
  }

  function renderInspector() {
    const snapshot = state.snapshot;
    const approvalCount = snapshot.pending_approval ? 1 : 0;
    elements.approvalCount.textContent = String(approvalCount);
    elements.activityCount.textContent = String(snapshot.run?.event_count ?? state.view.activity.length);
    elements.auditCount.textContent = String(snapshot.audit_entries ?? snapshot.audit.length);
    const approvalTab = document.querySelector("#tab-approval");
    approvalTab.classList.toggle("has-pending", approvalCount > 0);

    for (const tab of elements.inspectorTabs.querySelectorAll("[role=tab]")) {
      const selected = tab.dataset.tab === state.view.inspectorTab;
      tab.setAttribute("aria-selected", String(selected));
      tab.tabIndex = selected ? 0 : -1;
    }
    elements.approvalPanel.hidden = state.view.inspectorTab !== "approval";
    elements.activityPanel.hidden = state.view.inspectorTab !== "activity";
    elements.auditPanel.hidden = state.view.inspectorTab !== "audit";
    renderApproval();
    renderActivity();
    renderAudit();
  }

  function renderApproval() {
    const approval = state.snapshot.approval;
    if (!approval) {
      const empty = emptyState(
        "没有待审批动作",
        "Ask 模式下，未命中信任规则的调用会显示在这里。Plan 的能力边界不能通过审批越过。",
      );
      if (state.snapshot.run?.execution_mode === "plan" && state.snapshot.run?.approval_mode === "auto") {
        empty.append(createElement("p", "mode-note", "Auto 只在 Plan 能力边界内自动允许。"));
      }
      elements.approvalPanel.replaceChildren(empty);
      return;
    }
    const card = createElement("article", "approval-card");
    const title = createElement("h3", "approval-title", approvalTitle(approval));
    title.id = "pending-approval-title";
    title.tabIndex = -1;
    card.append(title, createElement("p", "meta-line", `${approval.run_id} · ${approval.call_id}`));
    const facts = approval.facts;
    const factsContainer = createElement("div", "approval-facts");
    if (facts?.type === "file") {
      factsContainer.append(factRow("操作", stringOr(facts.operation, "unknown")));
      factsContainer.append(
        factCodeRow("完整路径", stringOr(facts.path, ""), nextId("approval-file-path")),
      );
      card.append(factsContainer);
      card.append(createElement("p", "approval-warning", "符号链接可能把逻辑路径映射到范围外目标；这里展示的是本次 resolved 逻辑路径。"));
    } else if (facts?.type === "shell") {
      const commandId = nextId("approval-command");
      factsContainer.append(
        factCodeRow("完整命令", stringOr(facts.command, ""), commandId),
        factRow("工作目录", stringOr(facts.workdir, ""), true),
        factRow("有效超时", `${numberOr(facts.timeout_ms, 0)} ms`),
        factRow("进程模式", shellProcessModeLabel(facts.process_mode)),
        factRow("输出上限", "stdout + stderr 合计 1 MiB"),
        factRow("环境", "继承父环境，已过滤敏感变量"),
      );
      card.append(factsContainer);
      card.append(createElement("p", "approval-warning", "Shell 可以读取当前用户有权访问的文件、网络和进程，也可以绕过结构化文件策略。"));
      if (facts.process_mode === "detached") {
        card.append(createElement("p", "approval-warning", "此调用完成后，后台进程不再由 Session 管理；Run 取消、Session reset 或 Demo 退出都不会停止它。启动成功也不代表服务健康，请根据 PID、日志或端口自行核验和停止。"));
      }
    } else {
      factsContainer.append(factRow("工具", stringOr(facts?.tool_name, approval.tool_name)));
      card.append(factsContainer);
    }
    card.append(createElement("p", "meta-line", "原因：未命中 Build 信任规则，仅决定本次调用。"));
    setError(card.appendChild(createElement("p", "inline-error")), state.view.operationError?.area === "approval" ? state.view.operationError.message : null);
    const actions = createElement("div", "button-row");
    const isSubmitting = state.view.operation === "approval";
    const deny = createElement("button", "button button-secondary", isSubmitting ? "正在提交决定…" : "Deny");
    deny.type = "button";
    deny.dataset.action = "decide-approval";
    deny.dataset.decision = "deny";
    deny.disabled = isSubmitting || state.connection.status !== "connected";
    const allow = createElement("button", "button button-primary", isSubmitting ? "正在提交决定…" : "Allow once");
    allow.type = "button";
    allow.dataset.action = "decide-approval";
    allow.dataset.decision = "allow_once";
    allow.disabled = deny.disabled;
    actions.append(deny, allow);
    card.append(actions);
    elements.approvalPanel.replaceChildren(card);
  }

  function renderActivity() {
    const fragment = document.createDocumentFragment();
    const run = state.snapshot.run;
    if (run?.last_guardrail) {
      const guardrail = createElement(
        "article",
        `guardrail-card guardrail-${run.last_guardrail.mode === "enforce" ? "enforce" : "observe"}`,
      );
      guardrail.setAttribute("role", run.last_guardrail.mode === "enforce" ? "alert" : "status");
      guardrail.append(
        createElement("strong", "", guardrailKindLabel(run.last_guardrail.kind)),
        createElement("p", "meta-line", guardrailSummary(run.last_guardrail)),
      );
      fragment.append(guardrail);
    }
    const list = createElement("div", "activity-list");
    for (const activity of state.view.activity.slice().reverse()) {
      const card = createElement("article", "activity-card");
      const time = createElement("time", "", formatTime(activity.timestamp));
      const suffix = activity.occurrences > 1 ? ` · ${activity.occurrences} 个分片` : "";
      const title = createElement("p", `status-${activity.tone}`, `${activity.title}${suffix}`);
      const details = detailsBlock("结构化详情", JSON.stringify(activity.detail, null, 2), "json-block");
      card.append(time, title, details);
      list.append(card);
    }
    if (!state.view.activity.length) {
      list.append(emptyState("暂无本页活动事件", "页面刷新后以 snapshot 恢复权威状态；新的 SSE 事件会从这里继续记录。"));
    }
    fragment.append(list);
    elements.activityPanel.replaceChildren(fragment);
  }

  function renderAudit() {
    const fragment = document.createDocumentFragment();
    const filters = createElement("div", "audit-filters");
    const filterLabels = [
      ["all", "全部"],
      ["file", "文件"],
      ["shell", "Shell"],
      ["problem", "拒绝/失败"],
    ];
    for (const [value, label] of filterLabels) {
      const button = createElement("button", "", label);
      button.type = "button";
      button.dataset.action = "audit-filter";
      button.dataset.filter = value;
      button.setAttribute("aria-pressed", String(state.view.auditFilter === value));
      filters.append(button);
    }
    fragment.append(filters);
    const entries = state.snapshot.audit.filter(matchesAuditFilter);
    for (const entry of entries.slice().reverse()) {
      fragment.append(renderAuditEntry(entry));
    }
    if (!entries.length) {
      fragment.append(emptyState("没有匹配的审计条目", "工具调用经过策略或审批后，会在这里显示 resolved 事实和执行结论。"));
    }
    elements.auditPanel.replaceChildren(fragment);
  }

  function renderAuditEntry(entry) {
    const card = createElement("article", "audit-card");
    const heading = createElement("div", "audit-heading");
    heading.append(
      createElement("strong", "", `${entry.tool_name} · ${toolStatusLabel(entry.status)}`),
      createElement("span", "sequence-label", `#${entry.sequence}`),
    );
    card.append(heading);
    card.append(createElement("p", "meta-line", `${entry.run_id} · ${entry.call_id} · ${formatTime(entry.timestamp_ms)}`));
    const facts = createElement("div", "tool-facts");
    if (entry.facts?.type === "file") {
      facts.append(factRow(entry.facts.operation, entry.facts.path, true));
    } else if (entry.facts?.type === "shell") {
      facts.append(factRow("workdir", entry.facts.workdir, true));
      facts.append(factRow("process mode", shellProcessModeLabel(entry.facts.process_mode)));
      facts.append(factRow("command", firstLine(entry.facts.command), true));
    } else {
      facts.append(factRow("tool", entry.facts?.tool_name || entry.tool_name));
    }
    card.append(facts);
    const flow = ["Resolved", `${entry.policy}: ${entry.rule}`];
    if (entry.approval_id) {
      flow.push(entry.decision ? `Approval ${entry.decision}` : "Waiting approval");
    }
    flow.push(toolStatusLabel(entry.status));
    card.append(createElement("p", "audit-flow", flow.join(" → ")));
    if (entry.error_class) {
      card.append(createElement("p", "inline-error", `${errorTitle(entry.error_class)} · ${entry.error_class}`));
    }
    if (entry.exit_code !== null && entry.exit_code !== undefined) {
      card.append(createElement("p", "meta-line", `Shell exit code：${entry.exit_code}`));
    }
    card.append(detailsBlock("原始审计对象", JSON.stringify(entry, null, 2), "json-block"));
    return card;
  }

  function renderComposer() {
    const snapshot = state.snapshot;
    const connected = state.connection.status === "connected";
    const run = snapshot.run;
    const active = snapshot.active_run;
    const pending = snapshot.pending_approval;
    const isBuildAuto = state.draft.executionMode === "build" && state.draft.approvalMode === "auto";
    const needsRiskConfirm = isBuildAuto && !state.draft.buildAutoConfirmed;
    const canSend =
      connected &&
      !active &&
      !pending &&
      state.view.operation !== "start" &&
      state.draft.message.trim().length > 0 &&
      !needsRiskConfirm;

    for (const button of document.querySelectorAll("[data-mode-group]")) {
      const selected =
        button.dataset.modeGroup === "execution"
          ? button.dataset.modeValue === state.draft.executionMode
          : button.dataset.modeValue === state.draft.approvalMode;
      button.setAttribute("aria-pressed", String(selected));
      button.disabled = !state.snapshot || state.connection.status === "loading";
    }
    elements.executionModeHelp.textContent =
      state.draft.executionMode === "plan"
        ? "只在只读工作目录和临时工作区能力边界内执行。"
        : "允许进入更广泛的文件与 Shell 策略。";
    elements.approvalModeHelp.textContent =
      state.draft.approvalMode === "ask"
        ? "未命中信任规则时请求一次性审批。"
        : "能力边界内未匹配调用自动允许。";
    setVisible(elements.buildAutoConfirmation, needsRiskConfirm);
    setVisible(elements.draftRiskBadge, isBuildAuto && state.draft.buildAutoConfirmed);
    elements.composer.classList.toggle("is-risk", isBuildAuto && state.draft.buildAutoConfirmed);
    setVisible(elements.planAutoNote, state.draft.executionMode === "plan" && state.draft.approvalMode === "auto");
    const draftDiffers = Boolean(
      active &&
        run &&
        (run.execution_mode !== state.draft.executionMode || run.approval_mode !== state.draft.approvalMode),
    );
    setVisible(elements.modeFreezeNote, draftDiffers);
    elements.messageInput.disabled = !connected || active || pending || state.view.operation === "start";
    if (elements.messageInput.value !== state.draft.message) {
      elements.messageInput.value = state.draft.message;
    }
    elements.sendButton.textContent =
      state.view.operation === "start"
        ? "正在启动…"
        : `以 ${modeLabel(state.draft.executionMode)} + ${modeLabel(state.draft.approvalMode)} 运行`;
    elements.sendButton.disabled = !canSend;
    setError(elements.composerError, state.view.operationError?.area === "composer" ? state.view.operationError.message : null);
  }

  function renderResetDialog() {
    elements.resetWorkspacePath.textContent = state.snapshot?.temporary_workspace || "";
    setError(elements.resetError, state.view.resetError);
    const isResetting = state.view.operation === "reset";
    elements.resetConfirmButton.disabled = isResetting || state.connection.status !== "connected";
    elements.resetConfirmButton.textContent = isResetting ? "正在重置…" : "重置并删除临时工作区";
    if (state.view.resetOpen && !elements.resetDialog.open) {
      elements.resetDialog.showModal();
      elements.resetCancelButton.focus();
    } else if (!state.view.resetOpen && elements.resetDialog.open) {
      elements.resetDialog.close();
    }
  }

  function focusPendingApproval() {
    if (!state.view.focusApproval) {
      return;
    }
    state.view.focusApproval = false;
    const active = document.activeElement;
    if (active === elements.messageInput || active?.matches("input, textarea, [contenteditable=true]")) {
      return;
    }
    requestAnimationFrame(() => document.querySelector("#pending-approval-title")?.focus());
  }

  function emptyState(title, description) {
    const section = createElement("section", "empty-state");
    section.append(createElement("strong", "", title), createElement("p", "", description));
    return section;
  }

  function factRow(label, value, isCode = false) {
    const row = createElement("div", "fact-row");
    row.append(createElement("span", "", label));
    const output = createElement(isCode ? "code" : "span", "fact-value", value);
    if (isCode) {
      output.tabIndex = 0;
    }
    row.append(output);
    return row;
  }

  function factCodeRow(label, value, id) {
    const row = createElement("div", "fact-row");
    row.append(createElement("span", "", label));
    const body = createElement("div", "");
    const code = createElement("pre", "code-block", value);
    code.id = id;
    code.tabIndex = 0;
    const copy = createElement("button", "button button-quiet", "复制");
    copy.type = "button";
    copy.dataset.action = "copy";
    copy.dataset.copyId = id;
    body.append(code, copy);
    row.append(body);
    return row;
  }

  function detailsBlock(label, text, className) {
    const details = createElement("details", "raw-details");
    details.append(createElement("summary", "", label));
    const pre = createElement("pre", className, text);
    pre.tabIndex = 0;
    details.append(pre);
    return details;
  }

  function nextId(prefix) {
    return `${prefix}-${renderSequence}-${Math.random().toString(36).slice(2, 8)}`;
  }

  function toolContentText(content) {
    if (!isObject(content)) {
      return "";
    }
    if (content.type === "text") {
      return stringOr(content.value, "");
    }
    if (content.type === "json") {
      return JSON.stringify(content.value, null, 2);
    }
    return "";
  }

  function toolDisplayStatus(result, audit, stream) {
    if (audit?.status === "waiting_approval") return "waiting_approval";
    if (audit?.status === "denied") return "denied";
    if (audit?.status === "cancelled") return "cancelled";
    if (audit?.status === "failed") return audit.error_class === "timeout" ? "timeout" : "failed";
    if (audit?.status === "completed") return "success";
    if (audit?.status === "started") return "running";
    if (stream?.status) return stream.status;
    if (result?.status === "success") return "success";
    if (result?.status === "error") return "failed";
    return audit?.status || "resolved";
  }

  function toolStatusLabel(status) {
    const labels = {
      resolved: "参数已解析",
      authorized: "策略允许",
      waiting_approval: "等待审批",
      denied: "已拒绝",
      started: "执行中",
      running: "执行中",
      completed: "成功",
      success: "成功",
      failed: "失败",
      timeout: "超时",
      cancelled: "已取消",
    };
    return labels[status] || stringOr(status, "未知");
  }

  function statusClass(status) {
    if (["completed", "success", "authorized"].includes(status)) return "status-success";
    if (["waiting_approval", "resolved"].includes(status)) return "status-waiting";
    if (["failed", "timeout", "denied"].includes(status)) return "status-failed";
    if (["started", "running"].includes(status)) return "status-running";
    return "status-neutral";
  }

  function toolKindLabel(type) {
    if (type === "file") return "文件";
    if (type === "shell") return "Shell";
    return "通用";
  }

  function connectionBadgeClass(status) {
    if (status === "connected") return "badge-connected";
    if (status === "disconnected") return "badge-error";
    if (status === "recovering") return "badge-warning";
    return "badge-neutral";
  }

  function runStatusLabel(status) {
    const labels = {
      running: "正在执行",
      completed: "已完成",
      failed: "已失败",
      cancelled: "已取消",
      compaction_required: "需要上下文压缩",
    };
    return labels[status] || stringOr(status, "未知");
  }

  function modeLabel(mode) {
    const labels = { plan: "Plan", build: "Build", ask: "Ask", auto: "Auto" };
    return labels[mode] || stringOr(mode, "Unknown");
  }

  function guardrailKindLabel(kind) {
    return kind === "repeated_invocation" ? "重复调用达到阈值" : "连续失败达到阈值";
  }

  function guardrailSummary(trigger) {
    const behavior = trigger.mode === "enforce" ? "已终止执行" : "当前仅观察，不会中止执行";
    return `${guardrailKindLabel(trigger.kind)} ${trigger.threshold}（实际 ${trigger.observed}）· ${behavior} · call ${trigger.call_id}`;
  }

  function finishReasonLabel(reason) {
    if (typeof reason === "string") return `finish: ${reason}`;
    if (isObject(reason)) return `finish: ${stringOr(reason.type, "unknown")}`;
    return "finish: unknown";
  }

  function approvalTitle(approval) {
    const type = approval.facts?.type;
    if (type === "file") return `文件操作 · ${stringOr(approval.facts.operation, "unknown")}`;
    if (type === "shell") return "Shell · 当前用户权限";
    return `通用工具 · ${stringOr(approval.tool_name, "unknown")}`;
  }

  function errorTitle(errorClass) {
    const labels = {
      invalid_input: "参数无效",
      file_not_found: "路径不存在",
      unsupported_encoding: "不支持的文本编码",
      unsupported_file_type: "不支持的文件类型",
      file_too_large: "文件超过工具上限",
      search_backend_unavailable: "ripgrep 不可用",
      policy_denied: "策略拒绝",
      approval_denied: "用户拒绝",
      timeout: "Shell 执行超时",
      cancelled: "Run 已取消",
      concurrent_modification: "文件已发生变化",
      io: "文件系统 I/O 错误",
      tool_error: "工具执行失败",
    };
    return labels[errorClass] || "工具执行失败";
  }

  function matchesAuditFilter(entry) {
    switch (state.view.auditFilter) {
      case "file":
        return entry.facts?.type === "file";
      case "shell":
        return entry.facts?.type === "shell";
      case "problem":
        return ["denied", "failed", "cancelled"].includes(entry.status);
      default:
        return true;
    }
  }

  function firstLine(value) {
    return stringOr(value, "").split(/\r?\n/, 1)[0];
  }

  function formatTime(timestamp) {
    const date = new Date(numberOr(timestamp, Date.now()));
    return Number.isNaN(date.getTime()) ? "--:--:--" : date.toLocaleTimeString([], { hour12: false });
  }

  function stringOr(value, fallback) {
    return typeof value === "string" ? value : fallback;
  }

  function numberOr(value, fallback) {
    return typeof value === "number" && Number.isFinite(value) ? value : fallback;
  }

  function arrayOr(value, fallback) {
    return Array.isArray(value) ? value : fallback;
  }

  async function requestJson(path, options = {}) {
    const response = await fetch(path, {
      cache: "no-store",
      credentials: "same-origin",
      ...options,
      headers: options.body ? { "content-type": "application/json", ...(options.headers || {}) } : options.headers,
    });
    let body = null;
    try {
      body = await response.json();
    } catch (_error) {
      body = null;
    }
    if (!response.ok) {
      const error = new Error(stringOr(body?.message, `HTTP ${response.status}`));
      error.status = response.status;
      error.code = body?.code;
      throw error;
    }
    return body;
  }

  async function loadSnapshot() {
    const snapshot = await requestJson("/api/snapshot");
    applySnapshot(snapshot);
    return snapshot;
  }

  async function initialConnect() {
    state.connection.status = "loading";
    render();
    try {
      await loadSnapshot();
      state.connection.status = "connected";
      state.connection.recoveryAttempts = 0;
      startEventStream();
    } catch (_error) {
      state.connection.status = "disconnected";
      elements.blockedTitle.textContent = "无法连接 Safety Demo";
      elements.blockedMessage.textContent = "本机 Safety Demo 尚未启动或已经停止。";
    }
    render();
  }

  function startEventStream() {
    closeEventStream();
    const source = new EventSource("/api/events");
    eventSource = source;
    source.addEventListener("notification", handleNotification);
    source.addEventListener("gap", handleGap);
    source.onopen = () => {
      if (eventSource !== source) return;
      state.connection.status = "connected";
      state.connection.recoveryAttempts = 0;
      render();
      // 再取一次 snapshot，封闭“首次快照完成、SSE 真正建立”之间的事件窗口。
      void reconcileAfterSubscribe(source);
    };
    source.onerror = () => {
      if (eventSource !== source) return;
      closeEventStream();
      void recoverSnapshot("SSE 连接中断");
    };
  }

  async function reconcileAfterSubscribe(source) {
    try {
      await loadSnapshot();
      if (eventSource === source) {
        render();
      }
    } catch (_error) {
      if (eventSource === source) {
        void recoverSnapshot("SSE 建立后无法校准 snapshot");
      }
    }
  }

  function closeEventStream() {
    if (eventSource) {
      eventSource.close();
      eventSource = null;
    }
  }

  function handleNotification(event) {
    try {
      const notification = JSON.parse(event.data);
      if (applyEvent(notification)) {
        if (isHighFrequencyStreamEvent(notification.kind)) {
          scheduleStreamRender();
        } else {
          render();
        }
        if (shouldRefreshSnapshot(notification.kind)) {
          scheduleSnapshotRefresh();
        }
      }
    } catch (_error) {
      void recoverSnapshot("事件序号缺失或内容无效");
    }
  }

  function handleGap() {
    void recoverSnapshot("SSE 事件出现 gap");
  }

  function isHighFrequencyStreamEvent(kind) {
    return (
      kind?.type === "run_progress" &&
      ["text_delta", "reasoning_delta", "tool_output"].includes(kind.detail?.type)
    );
  }

  function shouldRefreshSnapshot(kind) {
    if (kind?.type !== "run_progress") {
      return true;
    }
    return ["tool_proposed", "tool_started", "tool_completed", "guardrail_triggered"].includes(
      kind.detail?.type,
    );
  }

  function scheduleSnapshotRefresh() {
    if (refreshTimer !== null) {
      return;
    }
    refreshTimer = window.setTimeout(async () => {
      refreshTimer = null;
      if (state.connection.status !== "connected") {
        return;
      }
      try {
        await loadSnapshot();
        render();
      } catch (_error) {
        void recoverSnapshot("无法刷新权威 snapshot");
      }
    }, 120);
  }

  async function recoverSnapshot(_reason) {
    const generation = ++recoveryGeneration;
    closeEventStream();
    if (refreshTimer !== null) {
      clearTimeout(refreshTimer);
      refreshTimer = null;
    }
    state.connection.status = "recovering";
    render();
    for (let attempt = 1; attempt <= MAX_RECOVERY_ATTEMPTS; attempt += 1) {
      state.connection.recoveryAttempts = attempt;
      if (attempt > 1) {
        await delay(400 * attempt);
      }
      if (generation !== recoveryGeneration) {
        return;
      }
      try {
        await loadSnapshot();
        if (generation !== recoveryGeneration) {
          return;
        }
        state.connection.status = "connected";
        state.connection.recoveryAttempts = 0;
        startEventStream();
        render();
        return;
      } catch (_error) {
        // 有限次恢复失败后进入稳定断开状态，不让 EventSource 无限重连旧端口。
      }
    }
    if (generation === recoveryGeneration) {
      state.connection.status = "disconnected";
      render();
    }
  }

  function delay(milliseconds) {
    return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
  }

  async function requestRun() {
    if (elements.sendButton.disabled) return;
    state.view.operation = "start";
    state.view.operationError = null;
    render();
    try {
      const snapshot = await requestJson("/api/runs", {
        method: "POST",
        body: JSON.stringify({
          message: state.draft.message,
          execution_mode: state.draft.executionMode,
          approval_mode: state.draft.approvalMode,
        }),
      });
      applySnapshot(snapshot);
      state.draft.message = "";
      state.view.inspectorTab = "activity";
    } catch (error) {
      state.view.operationError = { area: "composer", message: actionErrorMessage(error, "无法启动 Run") };
      if (error.status === 409) {
        await recoverSnapshot("启动 Run 冲突");
      }
    } finally {
      state.view.operation = null;
      render();
    }
  }

  async function requestCancel() {
    state.view.operation = "cancel";
    state.view.operationError = null;
    render();
    try {
      const snapshot = await requestJson("/api/runs/current/cancel", { method: "POST" });
      applySnapshot(snapshot);
    } catch (error) {
      state.view.operationError = { area: "run", message: actionErrorMessage(error, "停止请求失败") };
      if (error.status === 409) {
        await recoverSnapshot("停止 Run 冲突");
      }
    } finally {
      state.view.operation = null;
      render();
    }
  }

  async function decideApproval(decision) {
    const approval = state.snapshot?.approval;
    if (!approval) return;
    state.view.operation = "approval";
    state.view.operationError = null;
    render();
    try {
      const snapshot = await requestJson(`/api/approvals/${encodeURIComponent(approval.approval_id)}/decision`, {
        method: "POST",
        body: JSON.stringify({ decision }),
      });
      applySnapshot(snapshot);
    } catch (error) {
      state.view.operationError = {
        area: "approval",
        message: error.status === 409 ? "该审批已结束，正在恢复最新状态。" : actionErrorMessage(error, "审批提交失败"),
      };
      if (error.status === 409) {
        await recoverSnapshot("审批已失效");
      }
    } finally {
      state.view.operation = null;
      render();
    }
  }

  async function resetSession() {
    state.view.operation = "reset";
    state.view.resetError = null;
    render();
    try {
      const snapshot = await requestJson("/api/session/reset", { method: "POST" });
      applySnapshot(snapshot);
      state.view.activity = [];
      state.view.resetOpen = false;
      state.view.inspectorTab = "audit";
      showToast("Session 已重置，旧临时工作区已删除。");
    } catch (error) {
      state.view.resetError = actionErrorMessage(error, "Session 重置失败");
      if (error.status === 409) {
        await recoverSnapshot("重置 Session 冲突");
      }
    } finally {
      state.view.operation = null;
      render();
    }
  }

  function actionErrorMessage(error, fallback) {
    return error instanceof Error && error.message ? `${fallback}：${error.message}` : fallback;
  }

  async function copyFromElement(id, button) {
    const source = document.getElementById(id);
    if (!source) return;
    try {
      await navigator.clipboard.writeText(source.textContent || "");
      showToast("已复制");
    } catch (_error) {
      const previous = button.textContent;
      button.textContent = "复制失败";
      window.setTimeout(() => {
        button.textContent = previous;
      }, 1600);
    }
  }

  function showToast(message) {
    elements.toast.textContent = message;
    setVisible(elements.toast, true);
    if (toastTimer !== null) clearTimeout(toastTimer);
    toastTimer = window.setTimeout(() => setVisible(elements.toast, false), 1800);
  }

  function selectInspectorTab(tab, focus = false) {
    if (!["approval", "activity", "audit"].includes(tab)) return;
    state.view.inspectorTab = tab;
    render();
    if (focus) document.querySelector(`[data-tab="${tab}"]`)?.focus();
  }

  function setMode(group, value) {
    state.view.operationError = null;
    if (group === "execution" && ["plan", "build"].includes(value)) {
      state.draft.executionMode = value;
    } else if (group === "approval" && ["ask", "auto"].includes(value)) {
      state.draft.approvalMode = value;
    }
    const isBuildAuto = state.draft.executionMode === "build" && state.draft.approvalMode === "auto";
    state.draft.buildAutoPending = isBuildAuto && !state.draft.buildAutoConfirmed;
    render();
  }

  function handleClick(event) {
    const target = event.target.closest("button");
    if (!target) return;
    if (target.id === "retry-button") {
      void initialConnect();
      return;
    }
    if (target.id === "send-button") {
      void requestRun();
      return;
    }
    if (target.id === "reset-confirm-button") {
      void resetSession();
      return;
    }
    if (target.dataset.modeGroup) {
      setMode(target.dataset.modeGroup, target.dataset.modeValue);
      return;
    }
    switch (target.dataset.action) {
      case "cancel-run":
        void requestCancel();
        break;
      case "open-reset":
        state.view.resetOpen = true;
        state.view.resetError = null;
        render();
        break;
      case "copy":
        void copyFromElement(target.dataset.copyId, target);
        break;
      case "toggle-output": {
        const output = document.getElementById(target.dataset.targetId);
        if (output) {
          const expanded = output.classList.toggle("is-expanded");
          target.textContent = expanded ? "收起输出" : "展开到页面";
        }
        break;
      }
      case "return-safe-mode":
        state.draft.executionMode = "plan";
        state.draft.approvalMode = "ask";
        state.draft.buildAutoPending = false;
        render();
        break;
      case "confirm-build-auto":
        state.draft.buildAutoConfirmed = true;
        state.draft.buildAutoPending = false;
        render();
        break;
      case "decide-approval":
        void decideApproval(target.dataset.decision);
        break;
      case "audit-filter":
        state.view.auditFilter = target.dataset.filter;
        render();
        break;
      default:
        if (target.dataset.tab) selectInspectorTab(target.dataset.tab);
        break;
    }
  }

  function handleInput(event) {
    if (event.target === elements.messageInput) {
      state.draft.message = elements.messageInput.value;
      state.view.operationError = null;
      renderComposer();
    }
  }

  function handleKeydown(event) {
    if (event.target === elements.messageInput && (event.ctrlKey || event.metaKey) && event.key === "Enter") {
      event.preventDefault();
      void requestRun();
      return;
    }
    const tab = event.target.closest?.("[role=tab]");
    if (!tab || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const tabs = Array.from(elements.inspectorTabs.querySelectorAll("[role=tab]"));
    let index = tabs.indexOf(tab);
    if (event.key === "Home") index = 0;
    else if (event.key === "End") index = tabs.length - 1;
    else index = (index + (event.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    selectInspectorTab(tabs[index].dataset.tab, true);
  }

  function handleDialogClose() {
    state.view.resetOpen = false;
    state.view.resetError = null;
  }

  document.addEventListener("click", handleClick);
  document.addEventListener("input", handleInput);
  document.addEventListener("keydown", handleKeydown);
  elements.resetDialog.addEventListener("close", handleDialogClose);
  window.addEventListener("beforeunload", () => {
    recoveryGeneration += 1;
    closeEventStream();
    if (refreshTimer !== null) clearTimeout(refreshTimer);
    cancelScheduledStreamRender();
  });

  void initialConnect();
})();
