const state = {
  token: "",
  connected: false,
  selectedSessionId: null,
  selectedSession: null,
  activeRunId: null,
  eventAbort: null,
  streamedRuns: new Map(),
  liveTools: new Map(),
  attachments: [],
  selectedAttachmentIds: new Set(),
  submitting: false,
  currentUsage: null,
  currentUsageStep: null,
  pendingApprovals: [],
};

const MAX_LIVE_TOOL_OUTPUT_CHARS = 12_000;
const MAX_RENDERED_HISTORY_MESSAGES = 400;
const MAX_RENDERED_TEXT_CHARS = 200_000;
const MAX_RENDERED_REASONING_CHARS = 120_000;
const MESSAGE_TAIL_THRESHOLD_PX = 48;
const EVENT_RECONNECT_DELAY_MS = 1_000;

const pendingStreamingRenders = new Set();
const pendingToolRenders = new Set();
let uiFrame = null;
let scrollOnNextFrame = false;

const byId = (id) => document.getElementById(id);
const status = byId("status");
const sessionList = byId("session-list");
const messageList = byId("message-list");
const conversationStatus = byId("conversation-status");
const childTasks = window.createChildTaskDemo({
  runtimeCommand,
  childConversation: childConversationSnapshot,
  setStatus: setConversationStatus,
});

function setConnected(connected, message) {
  state.connected = connected;
  status.textContent = message;
  status.className = `status ${connected ? "online" : "offline"}`;
  ["refresh", "create-session", "refresh-workspaces", "register-workspace"]
    .forEach((id) => { byId(id).disabled = !connected; });
  updateSessionControls();
}

function applySelectedSession(session) {
  state.selectedSession = session;
  state.activeRunId = session.active_run_id || null;
  byId("agent-variant").value = session.current_variant || "build";
  byId("approval-mode").value = session.approval_mode || "ask";
  updateSessionControls();
}

function setConversationStatus(message = "", error = false) {
  conversationStatus.textContent = message;
  conversationStatus.className = `conversation-status${error ? " error" : ""}`;
}

function formatTokenCount(value) {
  return Number.isFinite(value) ? value.toLocaleString("zh-CN") : "—";
}

function renderTokenUsage() {
  const usage = state.currentUsage;
  byId("usage-input").textContent = formatTokenCount(usage?.input_tokens);
  byId("usage-output").textContent = formatTokenCount(usage?.output_tokens);
  byId("usage-total").textContent = formatTokenCount(usage?.total_tokens);
  byId("usage-cached").textContent = formatTokenCount(usage?.cached_input_tokens);
  byId("usage-context").textContent = usage
    ? `最近一次模型请求${state.currentUsageStep ? ` · Step ${state.currentUsageStep}` : ""}`
    : "暂无用量";
}

// Conversation 尾部可能是 User/Tool，或者在自动压缩后只保留 Context Summary。
// “最近一次模型请求”应取最后一条实际携带 usage 的模型消息，不能由尾部角色推断。
function latestUsageFromConversation(conversation) {
  const messages = conversation.messages || [];
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.role !== "assistant" && message.role !== "context_summary") continue;
    if (message.turn?.usage) return message.turn.usage;
  }
  return null;
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  headers.set("Authorization", `Bearer ${state.token}`);
  const response = await fetch(path, { ...options, headers });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error?.message || `HTTP ${response.status}`);
  return body;
}

async function hostCommand(command) {
  const response = await api("/commands", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ request_id: crypto.randomUUID(), command }),
  });
  return response.result;
}

async function runtimeCommand(type, payload) {
  const result = await hostCommand({ scope: "runtime", payload: { type, payload } });
  return result.payload.payload;
}

async function conversationSnapshot(sessionId) {
  const result = await hostCommand({
    scope: "conversation_snapshot",
    payload: { session_id: sessionId },
  });
  return result.payload.conversation;
}

async function childConversationSnapshot(sessionId, childTaskId) {
  const result = await hostCommand({
    scope: "child_task_conversation_snapshot",
    payload: { session_id: sessionId, child_task_id: childTaskId },
  });
  return result.payload.conversation;
}

function isMessageTailVisible() {
  return messageList.scrollHeight - messageList.scrollTop - messageList.clientHeight
    <= MESSAGE_TAIL_THRESHOLD_PX;
}

function scheduleUiFrame(followTail = false) {
  scrollOnNextFrame ||= followTail;
  if (uiFrame !== null) return;
  uiFrame = requestAnimationFrame(flushUiFrame);
}

function flushUiFrame() {
  uiFrame = null;
  for (const streaming of pendingStreamingRenders) flushStreamingMessage(streaming, true);
  pendingStreamingRenders.clear();
  for (const activity of pendingToolRenders) flushLiveToolOutput(activity);
  pendingToolRenders.clear();
  if (scrollOnNextFrame) messageList.scrollTop = messageList.scrollHeight;
  scrollOnNextFrame = false;
}

function resetPendingUiWork() {
  if (uiFrame !== null) cancelAnimationFrame(uiFrame);
  uiFrame = null;
  scrollOnNextFrame = false;
  pendingStreamingRenders.clear();
  pendingToolRenders.clear();
}

function displayText(text, limit) {
  if (text.length <= limit) return text;
  return `${text.slice(0, limit)}\n\n[Demo 已省略其余 ${formatTokenCount(text.length - limit)} 个字符；完整内容仍保存在 Runtime Conversation 中]`;
}

function appendMessage(
  role,
  label,
  text,
  meta = "",
  files = [],
  reasoning = "",
  options = {},
) {
  const container = options.container || messageList;
  const isLiveContainer = container === messageList;
  const followTail = isLiveContainer && options.scroll !== false && isMessageTailVisible();
  if (isLiveContainer && messageList.classList.contains("empty")) {
    messageList.replaceChildren();
    messageList.classList.remove("empty");
  }
  const row = document.createElement("div");
  row.className = `message-row ${role}`;
  const card = document.createElement("div");
  card.className = "message-card";
  const heading = document.createElement("div");
  heading.className = "message-label";
  heading.textContent = label;
  const body = document.createElement("div");
  body.className = "message-body";
  const bodyText = document.createTextNode(text);
  if (text) body.append(bodyText);
  const reasoningPanel = document.createElement("div");
  reasoningPanel.className = "message-reasoning";
  reasoningPanel.hidden = !reasoning;
  const reasoningLabel = document.createElement("div");
  reasoningLabel.className = "message-reasoning-label";
  reasoningLabel.textContent = "Reasoning";
  const reasoningBody = document.createElement("pre");
  reasoningBody.className = "message-reasoning-body";
  const reasoningBodyText = document.createTextNode(reasoning);
  if (reasoning) reasoningBody.append(reasoningBodyText);
  reasoningPanel.append(reasoningLabel, reasoningBody);
  const detail = document.createElement("div");
  detail.className = "message-meta";
  detail.textContent = meta;
  detail.hidden = !meta;
  const fileList = document.createElement("div");
  fileList.className = "file-reference-list";
  fileList.hidden = !files.length;
  files.forEach((file) => {
    const reference = document.createElement("span");
    reference.className = "file-reference";
    reference.textContent = file.original_name;
    reference.title = file.readable_path;
    fileList.append(reference);
  });
  card.append(heading);
  if (role === "assistant") card.append(reasoningPanel);
  card.append(body, fileList, detail);
  row.append(card);
  container.append(row);
  if (isLiveContainer && options.scroll !== false) scheduleUiFrame(followTail);
  return {
    row,
    body,
    bodyText,
    detail,
    reasoningPanel,
    reasoningBody,
    reasoningBodyText,
  };
}

function renderConversation(conversation) {
  resetPendingUiWork();
  state.streamedRuns.clear();
  state.liveTools.clear();
  messageList.className = "message-list";
  const currentUsage = latestUsageFromConversation(conversation);
  const toolNames = new Map();
  const messages = conversation.messages || [];
  const visibleStart = Math.max(0, messages.length - MAX_RENDERED_HISTORY_MESSAGES);
  const fragment = document.createDocumentFragment();

  if (visibleStart > 0) {
    const notice = document.createElement("div");
    notice.className = "history-truncation";
    notice.textContent = `Demo 为保持长任务流畅，已省略更早的 ${formatTokenCount(visibleStart)} 条消息。`;
    fragment.append(notice);
    for (const message of messages.slice(0, visibleStart)) {
      if (message.role !== "assistant") continue;
      for (const part of message.turn?.parts || []) {
        if (part.type === "tool_call") toolNames.set(part.data.id, part.data.name);
      }
    }
  }

  for (const message of messages.slice(visibleStart)) {
    const turn = message.turn || {};
    if (message.role === "context_summary") {
      appendMessage(
        "assistant",
        "上下文摘要",
        displayText(turn.text || "（空摘要）", MAX_RENDERED_TEXT_CHARS),
        "Runtime 自动压缩",
        [],
        "",
        { container: fragment, scroll: false },
      );
      continue;
    }
    if (message.role === "user") {
      const parts = turn.parts || [];
      const text = parts
        .filter((part) => part.type === "text")
        .map((part) => part.data.text)
        .join("");
      const files = parts
        .filter((part) => part.type === "file_references")
        .flatMap((part) => part.data.files || []);
      if (text || files.length) {
        appendMessage(
          "user",
          "用户",
          displayText(text || "（仅附件）", MAX_RENDERED_TEXT_CHARS),
          "",
          files,
          "",
          { container: fragment, scroll: false },
        );
      }
      continue;
    }
    if (message.role === "assistant") {
      const parts = turn.parts || [];
      const text = parts
        .filter((part) => part.type === "text")
        .map((part) => part.data.text)
        .join("");
      const reasoning = parts
        .filter((part) => part.type === "reasoning")
        .map((part) => part.data.text)
        .join("");
      const tools = parts
        .filter((part) => part.type === "tool_call")
        .map((part) => {
          toolNames.set(part.data.id, part.data.name);
          return part.data.name;
        });
      const visibleText = text || (tools.length ? `调用工具：${tools.join("、")}` : "（无可展示正文）");
      const meta = text && tools.length ? `调用工具：${tools.join("、")}` : "";
      appendMessage(
        "assistant",
        "Assistant",
        displayText(visibleText, MAX_RENDERED_TEXT_CHARS),
        meta,
        [],
        displayText(reasoning, MAX_RENDERED_REASONING_CHARS),
        { container: fragment, scroll: false },
      );
      continue;
    }
    if (message.role === "tool") {
      const result = turn.result || {};
      const callId = result.call_id || "未知调用";
      const toolName = toolNames.get(callId) || "未知工具";
      appendMessage(
        "tool",
        "工具",
        toolName,
        `${callId} · ${result.status || "unknown"}`,
        [],
        "",
        { container: fragment, scroll: false },
      );
    }
  }

  messageList.replaceChildren(fragment);

  state.currentUsage = currentUsage;
  state.currentUsageStep = null;
  childTasks.setParentConversation(conversation);
  renderTokenUsage();

  if (!messageList.children.length) {
    messageList.classList.add("empty");
    messageList.textContent = "当前 Session 暂无消息";
  }
  scheduleUiFrame(true);
}

function ensureStreamingMessage(runId) {
  let streaming = state.streamedRuns.get(runId);
  if (!streaming) {
    streaming = appendMessage("assistant", "Assistant", "", "生成中");
    streaming.pendingText = [];
    streaming.pendingReasoning = [];
    streaming.pendingTextChars = 0;
    streaming.pendingReasoningChars = 0;
    streaming.renderedTextChars = 0;
    streaming.renderedReasoningChars = 0;
    streaming.omittedTextChars = 0;
    streaming.omittedReasoningChars = 0;
    state.streamedRuns.set(runId, streaming);
  }
  return streaming;
}

function appendStreamingText(streaming, chunks, pendingField, renderedField, node, parent) {
  if (!chunks.length) return;
  const delta = chunks.join("");
  chunks.length = 0;
  streaming[pendingField] = 0;
  if (delta) {
    if (node.parentNode !== parent) parent.append(node);
    node.appendData(delta);
    streaming[renderedField] += delta.length;
  }
}

function flushStreamingMessage(streaming, updateMeta) {
  pendingStreamingRenders.delete(streaming);
  appendStreamingText(
    streaming,
    streaming.pendingText,
    "pendingTextChars",
    "renderedTextChars",
    streaming.bodyText,
    streaming.body,
  );
  appendStreamingText(
    streaming,
    streaming.pendingReasoning,
    "pendingReasoningChars",
    "renderedReasoningChars",
    streaming.reasoningBodyText,
    streaming.reasoningBody,
  );
  if (streaming.renderedReasoningChars || streaming.omittedReasoningChars) {
    streaming.reasoningPanel.hidden = false;
  }
  if (!updateMeta) return;
  const omitted = streaming.omittedTextChars + streaming.omittedReasoningChars;
  streaming.detail.textContent = omitted
    ? `生成中 · Demo 已省略 ${formatTokenCount(omitted)} 个字符`
    : (streaming.renderedReasoningChars ? "Reasoning 生成中" : "生成中");
  streaming.detail.hidden = false;
}

function queueStreamingDelta(runId, kind, delta) {
  const followTail = isMessageTailVisible();
  const streaming = ensureStreamingMessage(runId);
  const reasoning = kind === "reasoning";
  const pending = reasoning ? streaming.pendingReasoning : streaming.pendingText;
  const pendingField = reasoning ? "pendingReasoningChars" : "pendingTextChars";
  const renderedField = reasoning ? "renderedReasoningChars" : "renderedTextChars";
  const omittedField = reasoning ? "omittedReasoningChars" : "omittedTextChars";
  const limit = reasoning ? MAX_RENDERED_REASONING_CHARS : MAX_RENDERED_TEXT_CHARS;
  const remaining = Math.max(0, limit - streaming[renderedField] - streaming[pendingField]);
  const visible = delta.slice(0, remaining);
  if (visible) {
    pending.push(visible);
    streaming[pendingField] += visible.length;
  }
  streaming[omittedField] += delta.length - visible.length;
  pendingStreamingRenders.add(streaming);
  scheduleUiFrame(followTail);
}

function closeStreamingStep(runId) {
  const streaming = state.streamedRuns.get(runId);
  if (!streaming) return;
  flushStreamingMessage(streaming, false);
  if (!streaming.body.textContent.trim() && !streaming.reasoningBody.textContent.trim()) {
    streaming.row.remove();
  } else {
    if (!streaming.body.textContent.trim()) streaming.body.textContent = "（无正文输出）";
    streaming.detail.textContent = "已提交工具调用";
    streaming.detail.hidden = false;
  }
  state.streamedRuns.delete(runId);
}

function ensureLiveTool(callId, toolName = "未知工具") {
  let activity = state.liveTools.get(callId);
  if (activity) return activity;
  const message = appendMessage("tool", "工具", toolName, `${callId} · proposed`);
  const output = document.createElement("pre");
  output.className = "tool-output";
  output.hidden = true;
  message.detail.after(output);
  activity = {
    ...message,
    output,
    stdout: "",
    stderr: "",
    pendingStdout: "",
    pendingStderr: "",
  };
  state.liveTools.set(callId, activity);
  return activity;
}

function updateLiveToolStatus(callId, statusText) {
  const activity = ensureLiveTool(callId);
  activity.detail.textContent = `${callId} · ${statusText}`;
  activity.detail.hidden = false;
}

function appendLiveToolOutput(callId, channel, chunk) {
  const followTail = isMessageTailVisible();
  const activity = ensureLiveTool(callId);
  const pendingField = channel === "stderr" ? "pendingStderr" : "pendingStdout";
  activity[pendingField] = `${activity[pendingField]}${chunk}`.slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
  pendingToolRenders.add(activity);
  scheduleUiFrame(followTail);
}

function flushLiveToolOutput(activity) {
  activity.stdout = `${activity.stdout}${activity.pendingStdout}`.slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
  activity.stderr = `${activity.stderr}${activity.pendingStderr}`.slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
  activity.pendingStdout = "";
  activity.pendingStderr = "";
  const sections = [];
  if (activity.stdout) sections.push(`stdout\n${activity.stdout}`);
  if (activity.stderr) sections.push(`stderr\n${activity.stderr}`);
  activity.output.textContent = sections.join("\n\n");
  activity.output.hidden = !sections.length;
}

async function connect() {
  state.token = byId("token").value.trim();
  if (!state.token) return setConnected(false, "请输入 Token");
  try {
    const [health, capabilities] = await Promise.all([api("/health"), api("/capabilities")]);
    byId("capabilities").textContent = JSON.stringify({ health, capabilities }, null, 2);
    byId("token").value = "";
    setConnected(true, "已连接");
    startEvents();
    await Promise.all([loadWorkspaces(), loadSessions()]);
  } catch (error) {
    state.token = "";
    setConnected(false, error.message);
  }
}

async function loadWorkspaces() {
  try {
    const result = await runtimeCommand("list_workspaces", {});
    renderWorkspaces(result.workspaces || []);
  } catch (error) {
    setConversationStatus(`Workspace 查询失败：${error.message}`, true);
  }
}

function renderWorkspaces(workspaces) {
  const list = byId("workspace-list");
  list.className = workspaces.length ? "list" : "list empty";
  list.replaceChildren();
  const select = byId("workspace-select");
  const previous = select.value;
  select.replaceChildren(new Option("不绑定 Workspace", ""));
  if (!workspaces.length) {
    list.textContent = "暂无 Workspace";
    return;
  }
  workspaces.forEach((workspace) => {
    const item = document.createElement("div");
    item.className = "item";
    const text = document.createElement("div");
    const title = document.createElement("strong");
    title.textContent = workspace.user_directory;
    const detail = document.createElement("small");
    detail.textContent = `${workspace.workspace_id} · Agent 私有目录：${workspace.agent_directory}`;
    text.append(title, detail);
    item.append(text);
    list.append(item);
    select.append(new Option(workspace.user_directory, workspace.workspace_id));
  });
  if ([...select.options].some((option) => option.value === previous)) select.value = previous;
}

async function registerWorkspace() {
  const path = byId("workspace-path").value.trim();
  if (!path) return;
  try {
    const result = await runtimeCommand("register_workspace", { path });
    byId("workspace-path").value = "";
    await loadWorkspaces();
    byId("workspace-select").value = result.workspace.workspace_id;
  } catch (error) {
    setConversationStatus(`Workspace 登记失败：${error.message}`, true);
  }
}

async function loadSessions() {
  try {
    const result = await runtimeCommand("list_sessions", { filter: "all" });
    renderSessions(result.sessions || []);
  } catch (error) {
    setConversationStatus(`Session 查询失败：${error.message}`, true);
  }
}

function renderSessions(sessions) {
  sessionList.className = sessions.length ? "list" : "list empty";
  sessionList.replaceChildren();
  if (!sessions.length) {
    sessionList.textContent = "暂无 Session";
    return;
  }
  sessions.forEach((session) => {
    const item = document.createElement("div");
    item.className = `item selectable${session.session_id === state.selectedSessionId ? " selected" : ""}`;
    const text = document.createElement("div");
    const title = document.createElement("strong");
    title.textContent = session.title;
    const detail = document.createElement("small");
    detail.textContent = `${session.session_id} · ${session.model_key} · ${session.lifecycle} · ${session.current_variant || "build"}/${session.approval_mode || "ask"}${session.workspace_id ? ` · ${session.workspace_id}` : ""}`;
    text.append(title, detail);
    const badge = document.createElement("small");
    badge.textContent = `${session.message_count} messages`;
    item.append(text, badge);
    item.addEventListener("click", () => selectSession(session));
    sessionList.append(item);
    if (session.session_id === state.selectedSessionId) {
      applySelectedSession(session);
    }
  });
}

async function createSession() {
  const modelKey = byId("model-key").value.trim();
  const workspaceId = byId("workspace-select").value;
  const payload = { title: byId("session-title").value.trim() || null };
  if (modelKey) payload.model_key = modelKey;
  if (workspaceId) payload.workspace_id = workspaceId;
  try {
    const result = await runtimeCommand("create_session", payload);
    await loadSessions();
    await selectSession(result.session);
  } catch (error) {
    setConversationStatus(`Session 创建失败：${error.message}`, true);
  }
}

async function selectSession(session) {
  state.selectedSessionId = session.session_id;
  applySelectedSession(session);
  state.activeRunId = session.active_run_id || null;
  state.selectedAttachmentIds.clear();
  state.currentUsage = null;
  state.currentUsageStep = null;
  childTasks.reset(session.session_id);
  renderTokenUsage();
  byId("selected-session").textContent = `${session.title} · ${session.session_id}`;
  updateSessionControls();
  setConversationStatus("正在加载历史消息…");
  await Promise.all([
    loadSessions(),
    loadRuns(),
    loadConversation(session.session_id),
    loadAttachments(session.session_id),
    loadPendingApprovals(session.session_id),
  ]);
}

function updateSessionControls() {
  const selected = state.connected && Boolean(state.selectedSessionId);
  const active = state.selectedSession?.lifecycle === "active";
  const busy = Boolean(state.activeRunId) || state.submitting;
  byId("submit").disabled = !selected || !active || busy;
  byId("attachment-files").disabled = !selected || !active || busy;
  byId("archive-session").disabled = !selected || !active || busy;
  byId("restore-session").disabled = !selected || active;
  byId("cancel-run").disabled = !state.activeRunId;
  byId("agent-variant").disabled = !selected || !active;
  byId("approval-mode").disabled = !selected || !active;
  byId("reload-permissions").disabled = !selected;
  byId("refresh-approvals").disabled = !selected;
  byId("refresh-child-tasks").disabled = !selected;
  byId("quick-plan").disabled = !selected || !active || busy;
  byId("quick-build").disabled = !selected || !active || busy;
}

async function setSessionVariant(variant) {
  if (!state.selectedSessionId) return;
  const previous = state.selectedSession?.current_variant || "build";
  try {
    const result = await runtimeCommand("set_session_variant", {
      session_id: state.selectedSessionId,
      variant,
    });
    applySelectedSession(result.session);
    await loadSessions();
    return true;
  } catch (error) {
    byId("agent-variant").value = previous;
    setConversationStatus(`变体切换失败：${error.message}`, true);
    return false;
  }
}

async function setSessionApprovalMode(approvalMode) {
  if (!state.selectedSessionId) return;
  const previous = state.selectedSession?.approval_mode || "ask";
  try {
    const result = await runtimeCommand("set_session_approval_mode", {
      session_id: state.selectedSessionId,
      approval_mode: approvalMode,
    });
    applySelectedSession(result.session);
    await loadSessions();
  } catch (error) {
    byId("approval-mode").value = previous;
    setConversationStatus(`审批模式切换失败：${error.message}`, true);
  }
}

async function reloadPermissions() {
  if (!state.selectedSessionId) return;
  const button = byId("reload-permissions");
  button.disabled = true;
  try {
    const result = await runtimeCommand("reload_permissions", {
      session_id: state.selectedSessionId,
    });
    const fileSummary = (result.files || [])
      .map((file) => `${file.scope}:${file.status}`)
      .join(" · ");
    const diagnostics = (result.diagnostics || [])
      .map((diagnostic) => diagnostic.message || diagnostic.code)
      .join("；");
    byId("permission-status").textContent = result.applied
      ? `已应用${fileSummary ? ` · ${fileSummary}` : ""}`
      : `未应用${diagnostics ? ` · ${diagnostics}` : ""}`;
    setConversationStatus(result.applied ? "权限规则已重载。" : "权限规则无效，继续使用旧快照。", !result.applied);
  } catch (error) {
    byId("permission-status").textContent = "重载失败";
    setConversationStatus(`权限重载失败：${error.message}`, true);
  } finally {
    updateSessionControls();
  }
}

function approvalSubjectText(subject = {}) {
  if (subject.type === "file") {
    return `${subject.tool_name} · ${subject.operation} · ${subject.path}`;
  }
  if (subject.type === "shell") {
    return `${subject.tool_name} · ${subject.command}\n工作目录：${subject.working_directory}\n超时：${subject.timeout_ms} ms · ${subject.process_mode}`;
  }
  return subject.tool_name || "未知工具";
}

const APPROVAL_DECISION_LABELS = {
  allow_once: "本次允许",
  allow_session: "本 Session 允许",
  allow_workspace: "本 Workspace 允许",
  deny: "拒绝",
};

function renderPendingApprovals() {
  const list = byId("approval-list");
  list.replaceChildren();
  list.className = state.pendingApprovals.length ? "approval-list" : "approval-list empty";
  if (!state.pendingApprovals.length) {
    list.textContent = "当前没有待审批项";
    return;
  }
  state.pendingApprovals.forEach((approval) => {
    const item = document.createElement("article");
    item.className = "approval-item";
    const title = document.createElement("strong");
    title.textContent = approvalSubjectText(approval.subject);
    const detail = document.createElement("small");
    detail.textContent = `${approval.approval_id} · ${approval.variant}/${approval.approval_mode} · Run ${approval.run_id}${approval.child_task_id ? ` · Child ${approval.child_task_id}` : ""}`;
    const preview = document.createElement("pre");
    preview.className = "approval-facts";
    preview.textContent = `持久规则预览\n${approvalSubjectText(approval.exact_rule_preview)}`;
    const actions = document.createElement("div");
    actions.className = "approval-actions";
    (approval.available_decisions || []).forEach((decision) => {
      const button = document.createElement("button");
      button.className = decision === "deny" ? "danger" : "secondary";
      button.textContent = APPROVAL_DECISION_LABELS[decision] || decision;
      button.addEventListener("click", () => decideApproval(approval, decision));
      actions.append(button);
    });
    item.append(title, detail, preview, actions);
    list.append(item);
  });
}

async function loadPendingApprovals(sessionId = state.selectedSessionId) {
  if (!sessionId) {
    state.pendingApprovals = [];
    renderPendingApprovals();
    return;
  }
  try {
    const result = await runtimeCommand("list_pending_approvals", { session_id: sessionId });
    if (state.selectedSessionId !== sessionId) return;
    state.pendingApprovals = result.approvals || [];
    renderPendingApprovals();
  } catch (error) {
    if (state.selectedSessionId === sessionId) {
      setConversationStatus(`审批查询失败：${error.message}`, true);
    }
  }
}

async function decideApproval(approval, decision) {
  try {
    await runtimeCommand("decide_approval", {
      session_id: approval.session_id,
      approval_id: approval.approval_id,
      decision,
    });
    await loadPendingApprovals(approval.session_id);
    setConversationStatus(`审批已提交：${APPROVAL_DECISION_LABELS[decision] || decision}`);
  } catch (error) {
    setConversationStatus(`审批失败：${error.message}`, true);
    await loadPendingApprovals(approval.session_id);
  }
}

async function prepareQuickInput(variant, text) {
  if (!await setSessionVariant(variant)) return;
  byId("message").value = text;
  byId("message").focus();
}

async function loadConversation(sessionId = state.selectedSessionId) {
  if (!sessionId) return;
  try {
    const conversation = await conversationSnapshot(sessionId);
    if (state.selectedSessionId !== sessionId) return;
    renderConversation(conversation);
    setConversationStatus("");
  } catch (error) {
    if (state.selectedSessionId === sessionId) {
      setConversationStatus(`历史消息加载失败：${error.message}`, true);
    }
  }
}

async function loadRuns(sessionId = state.selectedSessionId) {
  if (!sessionId) return;
  try {
    const result = await runtimeCommand("list_runs", { session_id: sessionId });
    if (state.selectedSessionId !== sessionId) return;
    const runs = result.runs || [];
    const active = runs.find((run) => ["accepted", "running", "cancelling"].includes(run.status));
    state.activeRunId = active?.run_id || null;
    updateSessionControls();
    await childTasks.refresh(runs.map((run) => run.run_id));
  } catch (error) {
    if (state.selectedSessionId === sessionId) {
      setConversationStatus(`Run 查询失败：${error.message}`, true);
    }
  }
}

async function loadAttachments(sessionId = state.selectedSessionId) {
  if (!sessionId) return;
  try {
    const result = await runtimeCommand("list_attachments", { session_id: sessionId });
    if (state.selectedSessionId !== sessionId) return;
    state.attachments = result.attachments || [];
    const availableIds = new Set(state.attachments.map((attachment) => attachment.attachment_id));
    state.selectedAttachmentIds = new Set(
      [...state.selectedAttachmentIds].filter((attachmentId) => availableIds.has(attachmentId)),
    );
    renderAttachments();
  } catch (error) {
    if (state.selectedSessionId === sessionId) {
      setConversationStatus(`附件查询失败：${error.message}`, true);
    }
  }
}

function renderAttachments() {
  const list = byId("attachment-list");
  list.replaceChildren();
  list.className = state.attachments.length ? "file-list" : "file-list empty";
  if (!state.attachments.length) {
    list.textContent = "当前 Session 暂无附件";
    return;
  }
  const canSelect = state.selectedSession?.lifecycle === "active"
    && !state.activeRunId
    && !state.submitting;
  state.attachments.forEach((attachment) => {
    const label = document.createElement("label");
    label.className = "file-item";
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.checked = state.selectedAttachmentIds.has(attachment.attachment_id);
    checkbox.disabled = !canSelect || attachment.state !== "ready";
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) state.selectedAttachmentIds.add(attachment.attachment_id);
      else state.selectedAttachmentIds.delete(attachment.attachment_id);
    });
    const name = document.createElement("span");
    name.textContent = attachment.original_name;
    name.title = attachment.agent_readable_path;
    const detail = document.createElement("small");
    detail.textContent = `${attachment.state} · ${attachment.size_bytes} bytes`;
    label.append(checkbox, name, detail);
    list.append(label);
  });
}

function renderPendingFiles() {
  const files = [...byId("attachment-files").files];
  const list = byId("pending-files");
  list.replaceChildren();
  list.className = files.length ? "file-list" : "file-list empty";
  if (!files.length) {
    list.textContent = "未选择新文件";
    return;
  }
  files.forEach((file) => {
    const item = document.createElement("div");
    item.className = "file-item";
    const name = document.createElement("span");
    name.textContent = file.name;
    const detail = document.createElement("small");
    detail.textContent = `${file.size} bytes · 待上传`;
    item.append(name, detail);
    list.append(item);
  });
}

async function uploadPendingFiles() {
  const attachmentIds = [];
  for (const file of byId("attachment-files").files) {
    const form = new FormData();
    form.append("file", file, file.name);
    const result = await api(`/sessions/${encodeURIComponent(state.selectedSessionId)}/attachments`, {
      method: "POST",
      body: form,
    });
    attachmentIds.push(result.attachment.attachment_id);
    state.selectedAttachmentIds.add(result.attachment.attachment_id);
  }
  if (attachmentIds.length) await loadAttachments();
  return attachmentIds;
}

async function submitInput() {
  const message = byId("message").value;
  if (!state.selectedSessionId || !message.trim()) return;
  state.submitting = true;
  updateSessionControls();
  try {
    if (byId("attachment-files").files.length) setConversationStatus("正在上传附件…");
    await uploadPendingFiles();
    const attachmentIds = [...state.selectedAttachmentIds];
    const result = await runtimeCommand("submit_input", {
      session_id: state.selectedSessionId,
      message,
      variant: byId("agent-variant").value,
      attachment_ids: attachmentIds,
      idempotency_key: crypto.randomUUID(),
    });
    state.submitting = false;
    state.activeRunId = result.run.run_id;
    state.currentUsage = null;
    state.currentUsageStep = null;
    renderTokenUsage();
    byId("message").value = "";
    byId("attachment-files").value = "";
    renderPendingFiles();
    const files = state.attachments
      .filter((attachment) => attachmentIds.includes(attachment.attachment_id))
      .map((attachment) => ({
        original_name: attachment.original_name,
        readable_path: attachment.agent_readable_path,
      }));
    state.selectedAttachmentIds.clear();
    renderAttachments();
    updateSessionControls();
    appendMessage("user", "用户", message, "", files);
    ensureStreamingMessage(result.run.run_id);
    setConversationStatus("模型正在生成…");
  } catch (error) {
    state.submitting = false;
    await loadAttachments();
    updateSessionControls();
    setConversationStatus(`消息提交失败：${error.message}`, true);
  }
}

async function cancelRun() {
  if (!state.selectedSessionId || !state.activeRunId) return;
  try {
    await runtimeCommand("cancel_run", {
      session_id: state.selectedSessionId,
      run_id: state.activeRunId,
    });
    setConversationStatus("正在取消 Run…");
    await loadRuns();
  } catch (error) {
    setConversationStatus(`取消失败：${error.message}`, true);
  }
}

async function archiveSession() {
  if (!state.selectedSessionId) return;
  try {
    const result = await runtimeCommand("archive_session", {
      session_id: state.selectedSessionId,
    });
    state.selectedSession = result.session;
    updateSessionControls();
    renderAttachments();
    await loadSessions();
    setConversationStatus("Session 已归档；历史消息与附件仍可查看。");
  } catch (error) {
    setConversationStatus(`归档失败：${error.message}`, true);
  }
}

async function restoreSession() {
  if (!state.selectedSessionId) return;
  try {
    const result = await runtimeCommand("restore_session", {
      session_id: state.selectedSessionId,
    });
    state.selectedSession = result.session;
    updateSessionControls();
    renderAttachments();
    await loadSessions();
    setConversationStatus("Session 已恢复。");
  } catch (error) {
    setConversationStatus(`恢复失败：${error.message}`, true);
  }
}

async function handleRuntimeEvent(event) {
  if (event.type === "session_created") {
    await loadSessions();
    return;
  }
  if (event.type === "session_variant_changed" || event.type === "session_approval_mode_changed") {
    if (event.session.session_id === state.selectedSessionId) applySelectedSession(event.session);
    await loadSessions();
    return;
  }
  if (event.type === "approval_requested") {
    if (event.approval.session_id === state.selectedSessionId) await loadPendingApprovals();
    return;
  }
  if (event.session_id !== state.selectedSessionId) return;

  if (childTasks.handleEvent(event)) return;

  if (event.type === "approval_resolved" || event.type === "approval_cancelled") {
    await loadPendingApprovals(event.session_id);
    return;
  }
  if (event.type === "permission_reloaded") {
    byId("permission-status").textContent = `已重载 · ${(event.files || []).map((file) => `${file.scope}:${file.status}`).join(" · ")}`;
    return;
  }

  if (event.type === "text_delta") {
    queueStreamingDelta(event.run_id, "text", event.delta);
    return;
  }
  if (event.type === "reasoning_delta") {
    queueStreamingDelta(event.run_id, "reasoning", event.delta);
    return;
  }
  if (event.type === "tool_proposed") {
    closeStreamingStep(event.run_id);
    ensureLiveTool(event.call_id, event.tool_name);
    setConversationStatus(`正在调用工具：${event.tool_name}`);
    return;
  }
  if (event.type === "tool_started") {
    updateLiveToolStatus(event.call_id, "running");
    return;
  }
  if (event.type === "tool_output") {
    appendLiveToolOutput(event.call_id, event.channel, event.chunk);
    return;
  }
  if (event.type === "tool_completed") {
    updateLiveToolStatus(event.call_id, event.status);
    setConversationStatus(event.status === "completed" ? "工具调用完成，模型继续生成…" : "工具调用失败，模型正在处理结果…");
    return;
  }
  if (event.type === "usage_updated") {
    state.currentUsage = event.usage;
    state.currentUsageStep = event.step;
    childTasks.setParentUsage(event.run_id, event.step, event.usage);
    renderTokenUsage();
    return;
  }
  if (event.type === "model_attempt_started") {
    setConversationStatus(`模型请求 Attempt ${event.attempt}…`);
    return;
  }
  if (event.type === "model_attempt_failed") {
    const suffix = event.will_retry ? "，准备重试" : "";
    setConversationStatus(`模型请求 Attempt ${event.attempt} 失败：${event.kind}${suffix}`, !event.will_retry);
    return;
  }
  if (event.type === "model_retry_scheduled") {
    setConversationStatus(`将在 ${event.delay_ms.toLocaleString("zh-CN")} ms 后进行 Attempt ${event.next_attempt}…`);
    return;
  }
  if (event.type === "model_stream_established") {
    setConversationStatus(`模型流已建立 · Attempt ${event.attempt}`);
    return;
  }
  if (event.type === "run_accepted" || event.type === "run_started" || event.type === "run_cancelling") {
    if (event.type === "run_accepted") {
      state.currentUsage = null;
      state.currentUsageStep = null;
      renderTokenUsage();
    }
    state.activeRunId = event.run_id;
    updateSessionControls();
    byId("cancel-run").disabled = event.type === "run_cancelling";
    return;
  }
  if (event.type === "run_finished") {
    state.activeRunId = null;
    updateSessionControls();
    await Promise.all([
      loadConversation(event.session_id),
      loadRuns(),
      loadSessions(),
      loadAttachments(event.session_id),
      loadPendingApprovals(event.session_id),
    ]);
    if (event.status === "completed") {
      setConversationStatus("");
    } else {
      const diagnostic = event.error ? ` · ${event.error.code}: ${event.error.message}` : "";
      setConversationStatus(`Run 已结束：${event.status}${diagnostic}`, Boolean(event.error));
    }
  }
}

async function refreshAuthoritativeState() {
  const sessionId = state.selectedSessionId;
  const refreshes = [loadWorkspaces(), loadSessions()];
  if (sessionId) {
    refreshes.push(
      loadRuns(sessionId),
      loadConversation(sessionId),
      loadAttachments(sessionId),
      loadPendingApprovals(sessionId),
    );
  }
  await Promise.all(refreshes);
}

function waitForEventReconnect(signal) {
  return new Promise((resolve) => {
    const onAbort = () => {
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, EVENT_RECONNECT_DELAY_MS);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

async function consumeEventStream(response, controller) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) throw new Error("事件流已结束");
    buffer += decoder.decode(value, { stream: true });
    const blocks = buffer.split("\n\n");
    buffer = blocks.pop();
    for (const block of blocks) {
      const lines = block.split("\n");
      const eventName = lines.find((line) => line.startsWith("event:"))
        ?.slice(6).trim() || "message";
      const data = lines.filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trimStart()).join("\n");
      if (eventName === "stream_gap") {
        const gap = JSON.parse(data || "{}");
        throw new Error(`事件积压，已丢失 ${gap.dropped_events ?? "未知数量的"} 条事件`);
      }
      if (eventName !== "runtime_event" || !data) continue;
      try {
        await handleRuntimeEvent(JSON.parse(data));
      } catch {
        setConversationStatus("收到无法解析的 Runtime 事件", true);
      }
      if (controller.signal.aborted) return;
    }
  }
}

async function startEvents() {
  state.eventAbort?.abort();
  const controller = new AbortController();
  state.eventAbort = controller;
  let needsSnapshotRefresh = false;
  while (!controller.signal.aborted) {
    try {
      const response = await fetch("/events", {
        headers: { Authorization: `Bearer ${state.token}` },
        signal: controller.signal,
      });
      if (!response.ok || !response.body) throw new Error(`SSE HTTP ${response.status}`);
      if (needsSnapshotRefresh) {
        setConnected(false, "事件流已重连，正在同步权威状态…");
        await refreshAuthoritativeState();
        if (controller.signal.aborted) return;
      }
      setConnected(true, "已连接");
      await consumeEventStream(response, controller);
    } catch (error) {
      if (controller.signal.aborted) return;
      needsSnapshotRefresh = true;
      setConnected(false, "事件流断开，正在重连…");
      setConversationStatus(`SSE 断开：${error.message}；正在重新同步…`, true);
      await waitForEventReconnect(controller.signal);
    }
  }
}

byId("connect").addEventListener("click", connect);
byId("refresh-workspaces").addEventListener("click", loadWorkspaces);
byId("register-workspace").addEventListener("click", registerWorkspace);
byId("refresh").addEventListener("click", () => Promise.all([
  loadWorkspaces(),
  loadSessions(),
  loadRuns(),
  loadConversation(),
  loadAttachments(),
  loadPendingApprovals(),
]));
byId("create-session").addEventListener("click", createSession);
byId("submit").addEventListener("click", submitInput);
byId("cancel-run").addEventListener("click", cancelRun);
byId("archive-session").addEventListener("click", archiveSession);
byId("restore-session").addEventListener("click", restoreSession);
byId("attachment-files").addEventListener("change", renderPendingFiles);
byId("agent-variant").addEventListener("change", (event) => setSessionVariant(event.target.value));
byId("approval-mode").addEventListener("change", (event) => setSessionApprovalMode(event.target.value));
byId("reload-permissions").addEventListener("click", reloadPermissions);
byId("refresh-approvals").addEventListener("click", () => loadPendingApprovals());
byId("quick-plan").addEventListener("click", () => prepareQuickInput("plan", "请继续调整上面的计划。"));
byId("quick-build").addEventListener("click", () => prepareQuickInput("build", "请根据上面的计划开始实施。"));
