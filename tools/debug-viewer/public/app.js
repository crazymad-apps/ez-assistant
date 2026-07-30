"use strict";

const turnsEl = document.getElementById("turns");
const eventsEl = document.getElementById("events");
const connEl = document.getElementById("conn");
const tabsEl = document.getElementById("session-tabs");
const channelTabsEl = document.getElementById("channel-tabs");
const mainEl = document.getElementById("main");

// Harness correlation 为 <session>/<run>：Tab 按 session 聚合，流卡片仍按完整
// correlation 隔离，避免相邻 Run 的模型事件串卡。
const sessions = new Set();
const runs = new Map(); // correlation -> { openCard }
let activeSession = "all";
let activeChannel = "agent";
const EVENT_CAP = 300;

const es = new EventSource("/events");
es.onopen = () => setConn(true);
es.onerror = () => setConn(false);
es.onmessage = (e) => handle(JSON.parse(e.data));
es.addEventListener("gap", (e) => notice(`中间缺失 ${e.data} 条调试消息`));

document
  .querySelector('.session-tab[data-session="all"]')
  .addEventListener("click", () => switchSession("all"));

for (const tab of channelTabsEl.querySelectorAll(".channel-tab")) {
  tab.addEventListener("click", () => switchChannel(tab.dataset.channel));
}

document.getElementById("clear-btn").addEventListener("click", () => {
  if (activeSession === "all") {
    turnsEl.innerHTML = "";
    eventsEl.innerHTML = "";
    sessions.clear();
    runs.clear();
    tabsEl
      .querySelectorAll('.session-tab:not([data-session="all"])')
      .forEach((tab) => tab.remove());
  } else {
    const selector = `.entry[data-session="${CSS.escape(activeSession)}"]`;
    mainEl.querySelectorAll(selector).forEach((el) => el.remove());
    sessions.delete(activeSession);
    for (const correlation of runs.keys()) {
      if (sessionIdOfCorrelation(correlation) === activeSession) {
        runs.delete(correlation);
      }
    }
    tabsEl
      .querySelector(`.session-tab[data-session="${CSS.escape(activeSession)}"]`)
      ?.remove();
    switchSession("all");
  }
});

function setConn(open) {
  connEl.textContent = open ? "已连接" : "未连接";
  connEl.className = "conn " + (open ? "conn-open" : "conn-closed");
}

function correlationOf(msg) {
  return msg.correlation_id ?? "未关联";
}

function sessionIdOfCorrelation(correlation) {
  const separator = correlation.lastIndexOf("/");
  return separator > 0 ? correlation.slice(0, separator) : correlation;
}

function sessionOf(msg) {
  return sessionIdOfCorrelation(correlationOf(msg));
}

function ensureSession(id) {
  if (!sessions.has(id)) {
    sessions.add(id);
    addTab(id);
  }
}

function getRun(msg) {
  const correlation = correlationOf(msg);
  ensureSession(sessionOf(msg));
  if (!runs.has(correlation)) {
    runs.set(correlation, { openCard: null });
  }
  return runs.get(correlation);
}

function addTab(id) {
  const tab = document.createElement("button");
  tab.type = "button";
  tab.className = "session-tab";
  tab.dataset.session = id;
  tab.textContent = id;
  tab.addEventListener("click", () => switchSession(id));
  tabsEl.appendChild(tab);
}

function switchSession(id) {
  activeSession = id;
  for (const tab of tabsEl.children) {
    tab.classList.toggle("active", tab.dataset.session === id);
  }
  applyFilter();
}

function switchChannel(channel) {
  activeChannel = channel;
  for (const tab of channelTabsEl.children) {
    const isActive = tab.dataset.channel === channel;
    tab.classList.toggle("active", isActive);
    tab.setAttribute("aria-selected", String(isActive));
  }
  applyFilter();
}

function applyFilter() {
  for (const el of turnsEl.querySelectorAll(".entry")) {
    el.hidden = activeSession !== "all" && el.dataset.session !== activeSession;
  }
  for (const el of eventsEl.querySelectorAll(".entry")) {
    const isAnotherSession =
      activeSession !== "all" && el.dataset.session !== activeSession;
    el.hidden = isAnotherSession || el.dataset.channel !== activeChannel;
  }
}

function fmtTime(ms) {
  const d = new Date(ms);
  return (
    d.toLocaleTimeString("zh-CN", { hour12: false }) +
    "." +
    String(d.getMilliseconds()).padStart(3, "0")
  );
}

function fmtUsage(u) {
  if (!u) return "";
  const cached = u.cached_input_tokens != null ? ` · cached ${u.cached_input_tokens}` : "";
  const reasoning = u.reasoning_tokens != null ? ` · reasoning ${u.reasoning_tokens}` : "";
  return `tokens: ${u.input_tokens} in / ${u.output_tokens} out${cached}${reasoning}`;
}

function fmtError(err) {
  if (typeof err === "string") return err;
  const [kind, data] = Object.entries(err)[0];
  if (typeof data === "string") return `${kind}: ${data}`;
  return `${kind}: ${JSON.stringify(data)}`;
}

// FinishReason 是相邻标签枚举：{"type":"stop"} 或 {"type":"other","value":"..."}。
function fmtFinishReason(fr) {
  if (typeof fr === "string") return fr;
  if (fr == null) return "unknown";
  return fr.value != null ? `${fr.type}: ${fr.value}` : fr.type;
}

function handle(msg) {
  ensureSession(sessionOf(msg));
  const p = msg.payload;
  if (msg.ch === "agent" && p.kind === "agent_event") {
    onAgentEvent(msg, p.event);
    return;
  }
  if (msg.ch === "runtime" && p.kind === "runtime_event") {
    if (p.name === "user_message_appended") {
      addUserMessage(msg, p.data);
    }
    addTimeline(msg, "runtime", runtimeTitle(p.name, p.data), p.data);
    return;
  }
  if (msg.ch !== "llm") return;
  appendRaw(msg);

  if (p.kind === "turn_requested") {
    const card = newCard(msg, "live", "sending");
    card.head.main.textContent = "请求已发送，等待建立…";
    const details = document.createElement("details");
    const summary = document.createElement("summary");
    const count = p.request.conversation?.messages?.length ?? 0;
    summary.textContent = `请求快照 · ${count} 条消息`;
    const pre = document.createElement("pre");
    pre.className = "snapshot";
    pre.textContent = JSON.stringify(p.request, null, 2);
    details.append(summary, pre);
    card.body.appendChild(details);
    getRun(msg).openCard = card;
  } else if (p.kind === "turn_established") {
    const card = ensureCard(msg);
    card.badge.textContent = "streaming";
    card.head.main.textContent = `${p.model} · 建立 ${p.elapsed_ms}ms · ${p.message_count} 条消息`;
  } else if (p.kind === "establishment_failed") {
    const card = newCard(msg, "err", "建立失败");
    card.el.classList.add("failed");
    card.head.main.textContent = p.error;
  } else if (p.kind === "model_event") {
    const [type, data] = Object.entries(p.event)[0];
    onModelEvent(msg, type, data);
  }
}

function runtimeTitle(name, data) {
  if (name === "context_window_evaluated") {
    const evaluation = data?.evaluation ?? {};
    const decision = evaluation.decision?.type ?? evaluation.decision ?? "unknown";
    return `${name} · ${decision} · ${evaluation.used_tokens ?? "?"}/${evaluation.context_window_tokens ?? "?"}`;
  }
  if (name === "context_compaction_finished") {
    const cause = data?.report?.cause ?? "unknown";
    return `${name} · ${data?.outcome ?? "unknown"} · ${cause}`;
  }
  if (name === "continuation_started") {
    return `${name} · ${data?.previous_run_id ?? "?"} → ${data?.run_id ?? "?"}`;
  }
  if (name === "user_compaction_queued") {
    return `${name} · after active task`;
  }
  return name;
}

function addUserMessage(msg, data) {
  const message = data?.message;
  const text = (message?.parts ?? [])
    .filter((part) => part.type === "text")
    .map((part) => part.data?.text ?? "")
    .filter(Boolean)
    .join("\n");
  if (!text) return;

  const el = document.createElement("div");
  el.className = "user-message entry";
  el.dataset.session = sessionOf(msg);
  el.dataset.channel = "runtime";

  const head = document.createElement("div");
  head.className = "user-message-head";
  const label = document.createElement("span");
  label.className = "user-message-label";
  label.textContent = "用户输入";
  const meta = document.createElement("span");
  meta.className = "user-message-meta";
  meta.textContent = [data.run_id, message.id, fmtTime(msg.sent_at_ms)]
    .filter(Boolean)
    .join(" · ");
  head.append(label, meta);

  const body = document.createElement("div");
  body.className = "user-message-text";
  body.textContent = text;
  el.append(head, body);
  appendEntry(turnsEl, el);
}

function onAgentEvent(msg, event) {
  const type = event.type ?? "unknown";
  const detail = { ...event };
  delete detail.type;
  let title = type;
  if (type === "step_started") title = `${type} · step ${event.step}`;
  if (type === "usage_updated") title = `${type} · step ${event.step}`;
  if (type === "execution_compaction_required") {
    title = `${type} · step ${event.step} · ${event.reason ?? "unknown"}`;
  }
  if (type === "tool_started" || type === "tool_completed") {
    title = `${type} · ${event.call_id}`;
  }
  addTimeline(msg, "agent", title, detail);
}

function addTimeline(msg, channel, title, data) {
  const el = document.createElement("div");
  el.className = `timeline entry timeline-${channel}`;
  el.dataset.session = sessionOf(msg);
  el.dataset.channel = channel;

  const head = document.createElement("div");
  head.className = "timeline-head";
  const badge = document.createElement("span");
  badge.className = `layer-badge layer-${channel}`;
  badge.textContent = channel;
  const name = document.createElement("span");
  name.className = "timeline-name";
  name.textContent = title;
  const time = document.createElement("span");
  time.className = "timeline-time";
  time.textContent = fmtTime(msg.sent_at_ms);
  head.append(badge, name, time);
  el.appendChild(head);

  if (data != null && (typeof data !== "object" || Object.keys(data).length > 0)) {
    const detail = document.createElement("pre");
    detail.className = "timeline-data";
    detail.textContent = typeof data === "string" ? data : JSON.stringify(data, null, 2);
    el.appendChild(detail);
  }
  const raw = document.createElement("details");
  raw.className = "timeline-raw";
  const summary = document.createElement("summary");
  summary.textContent = "原始事件";
  const snapshot = document.createElement("pre");
  snapshot.className = "snapshot";
  snapshot.textContent = JSON.stringify(msg, null, 2);
  raw.append(summary, snapshot);
  el.appendChild(raw);
  appendEntry(eventsEl, el);
}

function onModelEvent(msg, type, d) {
  switch (type) {
    case "TurnStarted": {
      const card = ensureCard(msg);
      card.head.main.textContent = `${d.model.provider}/${d.model.model} · message ${d.message_id}`;
      break;
    }
    case "ReasoningStarted":
      addPart(msg, d.id, "reasoning");
      break;
    case "ReasoningDelta":
      appendDelta(msg, d.id, d.delta);
      break;
    case "TextStarted":
      addPart(msg, d.id, "text");
      break;
    case "TextDelta":
      appendDelta(msg, d.id, d.delta);
      break;
    case "ToolCallStarted":
      addToolPart(msg, d.id, d.name);
      break;
    case "ToolCallDelta":
      appendDelta(msg, d.id, d.arguments_delta);
      break;
    case "ToolCallFinished": {
      const part = findPart(msg, d.id);
      if (part) part.textContent = JSON.stringify(d.arguments, null, 2);
      break;
    }
    case "UsageUpdated": {
      ensureCard(msg).usage.textContent = fmtUsage(d.usage);
      break;
    }
    case "TurnFinished": {
      const card = ensureCard(msg);
      card.badge.textContent = `完成 · ${fmtFinishReason(d.message.finish_reason)}`;
      card.badge.className = "badge ok";
      if (d.message.usage) card.usage.textContent = fmtUsage(d.message.usage);
      getRun(msg).openCard = null;
      break;
    }
    case "TurnFailed": {
      const card = ensureCard(msg);
      card.el.classList.add("failed");
      card.badge.textContent = "失败";
      card.badge.className = "badge err";
      card.usage.textContent = fmtError(d.error);
      getRun(msg).openCard = null;
      break;
    }
  }
}

function newCard(msg, badgeClass, badgeText) {
  const el = document.createElement("div");
  el.className = "turn entry";
  el.dataset.session = sessionOf(msg);
  el.dataset.channel = "llm";
  const head = document.createElement("div");
  head.className = "turn-head";
  const badge = document.createElement("span");
  badge.className = "badge " + badgeClass;
  badge.textContent = badgeText;
  const main = document.createElement("span");
  main.className = "main";
  const time = document.createElement("span");
  time.textContent = fmtTime(msg.sent_at_ms);
  head.append(badge, main, time);
  const body = document.createElement("div");
  const usage = document.createElement("div");
  usage.className = "usage";
  el.append(head, body, usage);
  appendEntry(turnsEl, el);
  return { el, head: { main }, badge, body, usage, parts: new Map() };
}

function ensureCard(msg) {
  const run = getRun(msg);
  if (!run.openCard) run.openCard = newCard(msg, "live", "streaming");
  return run.openCard;
}

function addPart(msg, id, kind) {
  const card = ensureCard(msg);
  const el = document.createElement("div");
  el.className = `part part-${kind}`;
  card.body.appendChild(el);
  card.parts.set(String(id), el);
}

function addToolPart(msg, id, name) {
  const card = ensureCard(msg);
  const el = document.createElement("div");
  el.className = "part part-tool";
  const nameEl = document.createElement("div");
  nameEl.className = "tool-name";
  nameEl.textContent = `tool: ${name}`;
  const args = document.createElement("div");
  args.className = "tool-args";
  el.append(nameEl, args);
  card.body.appendChild(el);
  card.parts.set(String(id), args);
}

function appendDelta(msg, id, delta) {
  const part = findPart(msg, id);
  if (part) part.textContent += delta;
}

function findPart(msg, id) {
  const card = getRun(msg).openCard;
  return card ? card.parts.get(String(id)) : null;
}

function notice(text) {
  const el = document.createElement("div");
  el.className = "notice";
  el.textContent = text;
  turnsEl.appendChild(el);
}

function appendRaw(msg) {
  const el = document.createElement("div");
  el.className = "raw-entry entry";
  el.dataset.session = sessionOf(msg);
  el.dataset.channel = msg.ch ?? "unknown";
  el.textContent = JSON.stringify(msg);
  appendEntry(eventsEl, el);
}

function appendEntry(panel, el) {
  const sessionHidden = activeSession !== "all" && el.dataset.session !== activeSession;
  const channelHidden = panel === eventsEl && el.dataset.channel !== activeChannel;
  el.hidden = sessionHidden || channelHidden;
  panel.appendChild(el);
  if (panel === eventsEl) {
    trimChannelEntries(panel, el.dataset.channel);
  }
  if (!el.hidden && panel.scrollHeight - panel.scrollTop - panel.clientHeight < 120) {
    panel.scrollTop = panel.scrollHeight;
  }
}

function trimChannelEntries(panel, channel) {
  const entries = panel.querySelectorAll(
    `.entry[data-channel="${CSS.escape(channel)}"]`,
  );
  const overflow = entries.length - EVENT_CAP;
  for (let index = 0; index < overflow; index += 1) {
    entries[index].remove();
  }
}
