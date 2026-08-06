const view = {
  connectionIndicator: document.querySelector("#connection-indicator"),
  connectionStatus: document.querySelector("#connection-status"),
  configSummary: document.querySelector("#config-summary"),
  createSession: document.querySelector("#create-session"),
  sessionList: document.querySelector("#session-list"),
  memorySummary: document.querySelector("#memory-summary"),
  memoryList: document.querySelector("#memory-list"),
  memoryForm: document.querySelector("#memory-form"),
  memoryCategory: document.querySelector("#memory-category"),
  memoryContent: document.querySelector("#memory-content"),
  memoryAttributes: document.querySelector("#memory-attributes"),
  addMemory: document.querySelector("#add-memory"),
  sessionTitle: document.querySelector("#session-title"),
  sessionSequence: document.querySelector("#session-sequence"),
  messageList: document.querySelector("#message-list"),
  streamingCard: document.querySelector("#streaming-card"),
  streamingReasoning: document.querySelector("#streaming-reasoning"),
  streamingText: document.querySelector("#streaming-text"),
  runForm: document.querySelector("#run-form"),
  executionMode: document.querySelector("#execution-mode"),
  approvalMode: document.querySelector("#approval-mode"),
  messageInput: document.querySelector("#message-input"),
  submitRun: document.querySelector("#submit-run"),
  cancelRun: document.querySelector("#cancel-run"),
  actionError: document.querySelector("#action-error"),
  runInspector: document.querySelector("#run-inspector"),
  approvalCard: document.querySelector("#approval-card"),
  toolActivity: document.querySelector("#tool-activity"),
  auditList: document.querySelector("#audit-list"),
};

const state = {
  global: null,
  session: null,
  selectedSessionId: null,
  connected: false,
  calibrating: true,
};

let refreshTask = null;
let refreshRequested = false;

function setConnection(connected, label) {
  state.connected = connected;
  view.connectionIndicator.classList.toggle("connected", connected);
  view.connectionStatus.textContent = label;
  updateActions();
}

function setCalibrating(calibrating) {
  state.calibrating = calibrating;
  updateActions();
}

function updateActions() {
  const hasSession = state.session !== null;
  const active = state.session?.active_run === true;
  const writable = state.connected && !state.calibrating;
  view.createSession.disabled = !writable;
  view.memoryCategory.disabled = !writable;
  view.memoryContent.disabled = !writable;
  view.memoryAttributes.disabled = !writable;
  view.addMemory.disabled = !writable;
  for (const action of document.querySelectorAll("[data-memory-action]")) {
    action.disabled = !writable;
  }
  view.messageInput.disabled = !writable || !hasSession || active;
  view.executionMode.disabled = !writable || !hasSession || active;
  view.approvalMode.disabled = !writable || !hasSession || active;
  view.submitRun.disabled = !writable || !hasSession || active;
  view.cancelRun.disabled = !writable || !hasSession || !active;
}

async function api(path, options = {}) {
  const response = await fetch(path, {
    cache: "no-store",
    headers: options.body ? { "Content-Type": "application/json" } : undefined,
    ...options,
  });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      message = `${body.code}: ${body.message}`;
    } catch (_error) {
      // The HTTP status remains sufficient when an intermediary returns a non-JSON body.
    }
    throw new Error(message);
  }
  if (response.status === 204) {
    return null;
  }
  return response.json();
}

async function refreshAll() {
  refreshRequested = true;
  if (refreshTask !== null) {
    return refreshTask;
  }
  refreshTask = (async () => {
    setCalibrating(true);
    try {
      while (refreshRequested) {
        refreshRequested = false;
        await refreshSnapshotOnce();
      }
    } finally {
      setCalibrating(false);
      refreshTask = null;
    }
  })();
  return refreshTask;
}

async function refreshSnapshotOnce() {
    state.global = await api("/api/snapshot");
    if (
      state.selectedSessionId !== null &&
      !state.global.sessions.some(
        (session) => session.session_id === state.selectedSessionId,
      )
    ) {
      state.selectedSessionId = null;
      state.session = null;
    }
    if (state.selectedSessionId !== null) {
      state.session = await api(
        `/api/sessions/${encodeURIComponent(state.selectedSessionId)}/snapshot`,
      );
    }
    render();
}

async function selectSession(sessionId) {
  state.selectedSessionId = sessionId;
  try {
    await refreshAll();
  } catch (error) {
    showError(error);
    await refreshAll();
  }
}

function render() {
  renderConfig();
  renderSessions();
  renderMemory();
  renderConversation();
  renderInspector();
  updateActions();
}

function renderMemory() {
  view.memoryList.replaceChildren();
  const memory = state.global?.memory;
  if (memory === undefined) {
    view.memorySummary.textContent = "Store 加载中";
    return;
  }
  view.memorySummary.textContent = `revision ${memory.revision} · ${memory.entries.length} 条`;
  if (memory.entries.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-state";
    empty.textContent = "尚无常驻记忆";
    view.memoryList.append(empty);
    return;
  }
  for (const memoryEntry of memory.entries) {
    view.memoryList.append(renderMemoryEntry(memoryEntry));
  }
}

function renderMemoryEntry(memoryEntry) {
  const details = document.createElement("details");
  details.className = "memory-entry";
  const summary = document.createElement("summary");
  summary.textContent = `${memoryEntry.id} · ${memoryEntry.category} · ${memoryEntry.content}`;
  details.append(summary);

  const categoryLabel = document.createElement("label");
  categoryLabel.textContent = "归类";
  const category = document.createElement("input");
  category.value = memoryEntry.category;
  category.maxLength = 64;
  categoryLabel.append(category);

  const contentLabel = document.createElement("label");
  contentLabel.textContent = "正文";
  const content = document.createElement("textarea");
  content.value = memoryEntry.content;
  content.maxLength = 4096;
  contentLabel.append(content);

  const attributesLabel = document.createElement("label");
  attributesLabel.textContent = "属性 JSON";
  const attributes = document.createElement("input");
  attributes.value = JSON.stringify(memoryEntry.attributes);
  attributesLabel.append(attributes);

  const actions = document.createElement("div");
  actions.className = "memory-actions";
  const save = document.createElement("button");
  save.type = "button";
  save.dataset.memoryAction = "save";
  save.textContent = "保存";
  save.addEventListener("click", () => {
    void updateMemory(memoryEntry.id, category.value, content.value, attributes.value);
  });
  const remove = document.createElement("button");
  remove.type = "button";
  remove.dataset.memoryAction = "delete";
  remove.className = "danger";
  remove.textContent = "删除";
  remove.addEventListener("click", () => void deleteMemory(memoryEntry.id));
  actions.append(save, remove);
  details.append(categoryLabel, contentLabel, attributesLabel, actions);
  return details;
}

function renderConfig() {
  if (state.global === null) {
    view.configSummary.textContent = "配置加载中";
    return;
  }
  const config = state.global.config;
  const retry = config.retry_transient
    ? `retry on (${config.retries_scheduled} scheduled)`
    : "retry off";
  view.configSummary.textContent = `${config.provider}/${config.model} · ${config.connection_status} · ${config.context_window_tokens} tokens · reasoning ${config.reasoning_enabled ? "on" : "off"} · calls ${config.model_calls} / attempts ${config.model_attempts} · ${retry} · compaction ≤ ${config.max_compaction_handoffs} · ${config.persistence} · ${config.workdir}`;
}

function renderSessions() {
  view.sessionList.replaceChildren();
  const sessions = state.global?.sessions ?? [];
  if (sessions.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-state";
    empty.textContent = "还没有 Session。点击“新建”开始。";
    view.sessionList.append(empty);
    return;
  }
  for (const session of sessions) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "session-button";
    button.classList.toggle(
      "selected",
      session.session_id === state.selectedSessionId,
    );
    button.addEventListener("click", () => {
      void selectSession(session.session_id);
    });

    const title = document.createElement("span");
    title.textContent = session.title;
    const metadata = document.createElement("span");
    metadata.className = "session-meta";
    const status = session.active_run
      ? "running"
      : (session.last_status ?? "idle");
    metadata.textContent = `${session.session_id} · ${status} · seq ${session.sequence}`;
    button.append(title, metadata);
    view.sessionList.append(button);
  }
}

function renderConversation() {
  const shouldFollow = isMessageListNearBottom();
  view.messageList.replaceChildren();
  if (state.session === null) {
    view.sessionTitle.textContent = "请选择 Session";
    view.sessionSequence.textContent = "seq —";
    const empty = document.createElement("p");
    empty.className = "empty-state";
    empty.textContent = "新建或选择一个 Session 查看权威 Journal。";
    view.messageList.append(empty);
    view.streamingCard.hidden = true;
    return;
  }

  view.sessionTitle.textContent = state.session.title;
  view.sessionSequence.textContent = `seq ${state.session.sequence}`;
  if (state.session.journal.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-state";
    empty.textContent = "Journal 为空。";
    view.messageList.append(empty);
  } else {
    for (const message of state.session.journal) {
      view.messageList.append(renderMessage(message));
    }
  }

  const run = state.session.run;
  view.streamingCard.hidden = !state.session.active_run || run === null;
  view.streamingReasoning.textContent = run?.reasoning ?? "";
  view.streamingText.textContent = run?.text ?? "";
  if (shouldFollow) {
    view.messageList.scrollTop = view.messageList.scrollHeight;
  }
}

function isMessageListNearBottom() {
  const remaining =
    view.messageList.scrollHeight -
    view.messageList.scrollTop -
    view.messageList.clientHeight;
  return remaining < 72;
}

function renderMessage(message) {
  const article = document.createElement("article");
  article.className = `message ${message.role}`;
  const role = document.createElement("p");
  role.className = "message-role";
  role.textContent = message.role;
  article.append(role);

  if (message.role === "user") {
    const body = document.createElement("p");
    body.className = "message-body";
    body.textContent = partsText(message.turn.parts, "text");
    article.append(body);
  } else if (message.role === "assistant") {
    const reasoning = partsText(message.turn.parts, "reasoning");
    if (reasoning !== "") {
      const details = document.createElement("details");
      details.className = "reasoning";
      const summary = document.createElement("summary");
      summary.textContent = "Reasoning";
      const content = document.createElement("pre");
      content.textContent = reasoning;
      details.append(summary, content);
      article.append(details);
    }
    const body = document.createElement("p");
    body.className = "message-body";
    body.textContent = partsText(message.turn.parts, "text");
    article.append(body);
    const calls = Array.isArray(message.turn.parts)
      ? message.turn.parts.filter((part) => part.type === "tool_call")
      : [];
    for (const call of calls) {
      const tool = document.createElement("pre");
      tool.className = "message-body";
      tool.textContent = JSON.stringify(call.data, null, 2);
      article.append(tool);
    }
  } else {
    const body = document.createElement("pre");
    body.className = "message-body";
    body.textContent = JSON.stringify(message.turn, null, 2);
    article.append(body);
  }
  return article;
}

function partsText(parts, partType) {
  if (!Array.isArray(parts)) {
    return "";
  }
  return parts
    .filter((part) => part.type === partType && typeof part.data?.text === "string")
    .map((part) => part.data.text)
    .join("\n");
}

function renderInspector() {
  view.runInspector.replaceChildren();
  renderApproval();
  renderToolActivity();
  renderAudit();
  if (state.session !== null) {
    const frozen = state.session.frozen_prompt;
    addInspectorRow(
      "冻结 Prompt",
      `${frozen.part_count} parts · pinned revision ${frozen.pinned_revision} · ${frozen.pinned_entry_count} 条`,
    );
    addInspectorRow("Recall Sources", frozen.recall_sources.join(", "));
    addInspectorRow(
      "最新 Store",
      `revision ${state.global?.memory?.revision ?? "—"} · ${state.global?.memory?.entries?.length ?? "—"} 条`,
    );
  }
  const run = state.session?.run;
  if (run === null || run === undefined) {
    addInspectorRow("状态", "尚未运行");
    return;
  }
  addInspectorRow("Run", run.run_id);
  addInspectorRow("状态", run.status);
  addInspectorRow("执行 / 审批", `${run.execution_mode} / ${run.approval_mode}`);
  addInspectorRow("取消请求", String(run.cancel_requested));
  addInspectorRow("事件", String(run.event_count));
  addInspectorRow("Core 丢弃事件", String(run.dropped_events));
  addInspectorRow("Guardrail 触发", String(run.guardrail_triggers));
  addInspectorRow("上下文交接", String(run.compaction_handoffs));
  addInspectorRow("最近事件", run.last_event ?? "—");
  addInspectorRow("错误", run.last_error ?? "—");
  addInspectorRow(
    "Pending exchange",
    String(state.session.pending_exchange),
  );
  addInspectorRow("临时工作区", state.session.temporary_workspace);
}

function parseMemoryAttributes(text) {
  const attributes = JSON.parse(text);
  if (attributes === null || Array.isArray(attributes) || typeof attributes !== "object") {
    throw new Error("属性必须是 JSON 对象");
  }
  for (const value of Object.values(attributes)) {
    if (
      typeof value !== "string" &&
      !(typeof value === "number" && Number.isFinite(value))
    ) {
      throw new Error("属性值只能是字符串或有限数字");
    }
  }
  return attributes;
}

async function updateMemory(id, category, content, attributesText) {
  view.actionError.textContent = "";
  try {
    await api(`/api/memory/${encodeURIComponent(id)}`, {
      method: "POST",
      body: JSON.stringify({
        category,
        content,
        attributes: parseMemoryAttributes(attributesText),
      }),
    });
    await refreshAll();
  } catch (error) {
    showError(error);
  }
}

async function deleteMemory(id) {
  view.actionError.textContent = "";
  try {
    await api(`/api/memory/${encodeURIComponent(id)}/delete`, { method: "POST" });
    await refreshAll();
  } catch (error) {
    showError(error);
  }
}

function renderApproval() {
  view.approvalCard.replaceChildren();
  const approval = state.session?.approval;
  view.approvalCard.hidden = approval === null || approval === undefined;
  if (approval === null || approval === undefined) {
    return;
  }
  const title = document.createElement("h3");
  title.textContent = `待审批 · ${approval.tool_name}`;
  const facts = document.createElement("pre");
  facts.textContent = formatFacts(approval.facts);
  const actions = document.createElement("div");
  actions.className = "approval-actions";
  const allow = document.createElement("button");
  allow.type = "button";
  allow.className = "allow";
  allow.textContent = "仅本次允许";
  allow.addEventListener("click", () => void decideApproval(approval.approval_id, "allow_once"));
  const deny = document.createElement("button");
  deny.type = "button";
  deny.className = "danger";
  deny.textContent = "拒绝";
  deny.addEventListener("click", () => void decideApproval(approval.approval_id, "deny"));
  actions.append(allow, deny);
  view.approvalCard.append(title, facts, actions);
}

function renderToolActivity() {
  view.toolActivity.replaceChildren();
  const tools = state.session?.run?.tools ?? [];
  if (tools.length === 0) {
    view.toolActivity.textContent = "暂无工具调用";
    return;
  }
  for (const tool of tools) {
    const entry = document.createElement("article");
    entry.className = "tool-entry";
    const title = document.createElement("p");
    title.className = "entry-title";
    title.textContent = `${tool.tool_name} · ${tool.status}`;
    entry.append(title);
    if (tool.stdout !== "") {
      const stdout = document.createElement("pre");
      stdout.textContent = `stdout\n${tool.stdout}`;
      entry.append(stdout);
    }
    if (tool.stderr !== "") {
      const stderr = document.createElement("pre");
      stderr.textContent = `stderr\n${tool.stderr}`;
      entry.append(stderr);
    }
    view.toolActivity.append(entry);
  }
}

function renderAudit() {
  view.auditList.replaceChildren();
  const entries = state.session?.audit ?? [];
  if (entries.length === 0) {
    view.auditList.textContent = "暂无审计记录";
    return;
  }
  for (const audit of entries.slice().reverse()) {
    const entry = document.createElement("article");
    entry.className = "audit-entry";
    const title = document.createElement("p");
    title.className = "entry-title";
    title.textContent = `#${audit.sequence} ${audit.tool_name} · ${audit.status}`;
    const policy = document.createElement("p");
    policy.textContent = `${audit.policy} · ${audit.rule} · ${audit.decision ?? "pending"}`;
    const facts = document.createElement("pre");
    facts.textContent = formatFacts(audit.facts);
    entry.append(title, policy, facts);
    view.auditList.append(entry);
  }
}

function formatFacts(facts) {
  if (facts?.type === "file") {
    return `${facts.operation} ${facts.path}`;
  }
  if (facts?.type === "shell") {
    return `${facts.command}\nworkdir: ${facts.workdir}\ntimeout: ${facts.timeout_ms} ms · ${facts.process_mode}`;
  }
  return JSON.stringify(facts, null, 2);
}

async function decideApproval(approvalId, decision) {
  if (state.selectedSessionId === null) {
    return;
  }
  view.actionError.textContent = "";
  try {
    state.session = await api(
      `/api/sessions/${encodeURIComponent(state.selectedSessionId)}/approvals/${encodeURIComponent(approvalId)}/decision`,
      { method: "POST", body: JSON.stringify({ decision }) },
    );
    render();
  } catch (error) {
    showError(error);
    await refreshAll();
  }
}

function addInspectorRow(label, value) {
  const row = document.createElement("div");
  row.className = "inspector-row";
  const term = document.createElement("dt");
  term.textContent = label;
  const description = document.createElement("dd");
  description.textContent = value;
  row.append(term, description);
  view.runInspector.append(row);
}

function showError(error) {
  view.actionError.textContent =
    error instanceof Error ? error.message : String(error);
}

view.createSession.addEventListener("click", async () => {
  view.actionError.textContent = "";
  try {
    const session = await api("/api/sessions", {
      method: "POST",
      body: JSON.stringify({}),
    });
    state.selectedSessionId = session.session_id;
    state.session = session;
    await refreshAll();
    view.messageInput.focus();
  } catch (error) {
    showError(error);
  }
});

view.memoryForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  view.actionError.textContent = "";
  try {
    await api("/api/memory", {
      method: "POST",
      body: JSON.stringify({
        category: view.memoryCategory.value,
        content: view.memoryContent.value,
        attributes: parseMemoryAttributes(view.memoryAttributes.value),
      }),
    });
    view.memoryContent.value = "";
    await refreshAll();
  } catch (error) {
    showError(error);
  }
});

view.runForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (state.selectedSessionId === null) {
    return;
  }
  const message = view.messageInput.value.trim();
  if (message === "") {
    showError(new Error("请输入任务内容"));
    return;
  }
  view.actionError.textContent = "";
  try {
    state.session = await api(
      `/api/sessions/${encodeURIComponent(state.selectedSessionId)}/runs`,
      {
        method: "POST",
        body: JSON.stringify({
          message,
          execution_mode: view.executionMode.value,
          approval_mode: view.approvalMode.value,
        }),
      },
    );
    view.messageInput.value = "";
    render();
  } catch (error) {
    showError(error);
    await refreshAll();
  }
});

view.cancelRun.addEventListener("click", async () => {
  if (state.selectedSessionId === null) {
    return;
  }
  view.actionError.textContent = "";
  try {
    state.session = await api(
      `/api/sessions/${encodeURIComponent(state.selectedSessionId)}/runs/current/cancel`,
      { method: "POST" },
    );
    render();
  } catch (error) {
    showError(error);
    await refreshAll();
  }
});

const eventSource = new EventSource("/api/events");
eventSource.addEventListener("open", () => {
  setConnection(true, "已连接");
  void refreshAll().catch(showError);
});
eventSource.addEventListener("notification", (event) => {
  try {
    const notification = JSON.parse(event.data);
    const selectedChanged =
      notification.session_id === state.selectedSessionId;
    void refreshAll().then(() => {
      if (selectedChanged && state.session !== null) {
        view.sessionSequence.textContent = `seq ${state.session.sequence}`;
      }
    }).catch(showError);
  } catch (error) {
    showError(error);
    void refreshAll().catch(showError);
  }
});
eventSource.addEventListener("gap", () => {
  setConnection(true, "事件缺口，正在校准");
  void refreshAll()
    .then(() => setConnection(true, "已连接"))
    .catch(showError);
});
eventSource.addEventListener("error", () => {
  setConnection(false, "连接中断，等待重连");
});

render();
