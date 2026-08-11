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
};

const MAX_LIVE_TOOL_OUTPUT_CHARS = 12_000;
const EVENT_RECONNECT_DELAY_MS = 1_000;

const byId = (id) => document.getElementById(id);
const status = byId("status");
const sessionList = byId("session-list");
const messageList = byId("message-list");
const conversationStatus = byId("conversation-status");

function setConnected(connected, message) {
  state.connected = connected;
  status.textContent = message;
  status.className = `status ${connected ? "online" : "offline"}`;
  ["refresh", "create-session", "refresh-workspaces", "register-workspace"]
    .forEach((id) => { byId(id).disabled = !connected; });
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

function scrollMessages() {
  messageList.scrollTop = messageList.scrollHeight;
}

function appendMessage(role, label, text, meta = "", files = [], reasoning = "") {
  if (messageList.classList.contains("empty")) {
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
  body.textContent = text;
  const reasoningPanel = document.createElement("div");
  reasoningPanel.className = "message-reasoning";
  reasoningPanel.hidden = !reasoning;
  const reasoningLabel = document.createElement("div");
  reasoningLabel.className = "message-reasoning-label";
  reasoningLabel.textContent = "Reasoning";
  const reasoningBody = document.createElement("pre");
  reasoningBody.className = "message-reasoning-body";
  reasoningBody.textContent = reasoning;
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
  messageList.append(row);
  scrollMessages();
  return { row, body, detail, reasoningPanel, reasoningBody };
}

function renderConversation(conversation) {
  state.streamedRuns.clear();
  state.liveTools.clear();
  messageList.replaceChildren();
  messageList.className = "message-list";
  let currentUsage = null;
  const toolNames = new Map();

  for (const message of conversation.messages || []) {
    const turn = message.turn || {};
    if (message.role === "user") {
      currentUsage = null;
      const parts = turn.parts || [];
      const text = parts
        .filter((part) => part.type === "text")
        .map((part) => part.data.text)
        .join("");
      const files = parts
        .filter((part) => part.type === "file_references")
        .flatMap((part) => part.data.files || []);
      if (text || files.length) appendMessage("user", "用户", text || "（仅附件）", "", files);
      continue;
    }
    if (message.role === "assistant") {
      currentUsage = turn.usage || null;
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
      appendMessage("assistant", "Assistant", visibleText, meta, [], reasoning);
      continue;
    }
    if (message.role === "tool") {
      const result = turn.result || {};
      const callId = result.call_id || "未知调用";
      const toolName = toolNames.get(callId) || "未知工具";
      appendMessage("tool", "工具", toolName, `${callId} · ${result.status || "unknown"}`);
    }
  }

  state.currentUsage = currentUsage;
  state.currentUsageStep = null;
  renderTokenUsage();

  if (!messageList.children.length) {
    messageList.classList.add("empty");
    messageList.textContent = "当前 Session 暂无消息";
  }
  scrollMessages();
}

function ensureStreamingMessage(runId) {
  let streaming = state.streamedRuns.get(runId);
  if (!streaming) {
    streaming = appendMessage("assistant", "Assistant", "", "生成中");
    state.streamedRuns.set(runId, streaming);
  }
  return streaming;
}

function closeStreamingStep(runId) {
  const streaming = state.streamedRuns.get(runId);
  if (!streaming) return;
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
  activity = { ...message, output, stdout: "", stderr: "" };
  state.liveTools.set(callId, activity);
  return activity;
}

function updateLiveToolStatus(callId, statusText) {
  const activity = ensureLiveTool(callId);
  activity.detail.textContent = `${callId} · ${statusText}`;
  activity.detail.hidden = false;
}

function appendLiveToolOutput(callId, channel, chunk) {
  const activity = ensureLiveTool(callId);
  activity[channel] = `${activity[channel]}${chunk}`.slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
  const sections = [];
  if (activity.stdout) sections.push(`stdout\n${activity.stdout}`);
  if (activity.stderr) sections.push(`stderr\n${activity.stderr}`);
  activity.output.textContent = sections.join("\n\n");
  activity.output.hidden = !sections.length;
  scrollMessages();
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
    detail.textContent = `${session.session_id} · ${session.model_key} · ${session.lifecycle}${session.workspace_id ? ` · ${session.workspace_id}` : ""}`;
    text.append(title, detail);
    const badge = document.createElement("small");
    badge.textContent = `${session.message_count} messages`;
    item.append(text, badge);
    item.addEventListener("click", () => selectSession(session));
    sessionList.append(item);
    if (session.session_id === state.selectedSessionId) {
      state.selectedSession = session;
      state.activeRunId = session.active_run_id || null;
      updateSessionControls();
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
  state.selectedSession = session;
  state.activeRunId = session.active_run_id || null;
  state.selectedAttachmentIds.clear();
  state.currentUsage = null;
  state.currentUsageStep = null;
  renderTokenUsage();
  byId("selected-session").textContent = `${session.title} · ${session.session_id}`;
  updateSessionControls();
  setConversationStatus("正在加载历史消息…");
  await Promise.all([
    loadSessions(),
    loadRuns(),
    loadConversation(session.session_id),
    loadAttachments(session.session_id),
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
  if (event.session_id !== state.selectedSessionId) return;

  if (event.type === "text_delta") {
    const streaming = ensureStreamingMessage(event.run_id);
    streaming.body.textContent += event.delta;
    streaming.detail.textContent = "生成中";
    streaming.detail.hidden = false;
    scrollMessages();
    return;
  }
  if (event.type === "reasoning_delta") {
    const streaming = ensureStreamingMessage(event.run_id);
    streaming.reasoningBody.textContent += event.delta;
    streaming.reasoningPanel.hidden = false;
    streaming.detail.textContent = "Reasoning 生成中";
    streaming.detail.hidden = false;
    scrollMessages();
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
]));
byId("create-session").addEventListener("click", createSession);
byId("submit").addEventListener("click", submitInput);
byId("cancel-run").addEventListener("click", cancelRun);
byId("archive-session").addEventListener("click", archiveSession);
byId("restore-session").addEventListener("click", restoreSession);
byId("attachment-files").addEventListener("change", renderPendingFiles);
