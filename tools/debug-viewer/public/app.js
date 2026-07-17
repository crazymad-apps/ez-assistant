"use strict";

const turnsEl = document.getElementById("turns");
const rawEl = document.getElementById("raw");
const connEl = document.getElementById("conn");
const tabsEl = document.getElementById("session-tabs");
const mainEl = document.getElementById("main");

// 会话 = correlation_id；每条调试消息按会话归入 tab，「全部」展示混合流。
const sessions = new Map(); // id -> { openCard }
let activeSession = "all";
const RAW_CAP = 300;

const es = new EventSource("/events");
es.onopen = () => setConn(true);
es.onerror = () => setConn(false);
es.onmessage = (e) => handle(JSON.parse(e.data));
es.addEventListener("gap", (e) => notice(`中间缺失 ${e.data} 条调试消息`));

document
  .querySelector('.session-tab[data-session="all"]')
  .addEventListener("click", () => switchSession("all"));

document.getElementById("clear-btn").addEventListener("click", () => {
  if (activeSession === "all") {
    turnsEl.innerHTML = "";
    rawEl.innerHTML = "";
    sessions.clear();
    tabsEl
      .querySelectorAll('.session-tab:not([data-session="all"])')
      .forEach((tab) => tab.remove());
  } else {
    const selector = `.entry[data-session="${CSS.escape(activeSession)}"]`;
    mainEl.querySelectorAll(selector).forEach((el) => el.remove());
    sessions.delete(activeSession);
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

function sessionOf(msg) {
  return msg.correlation_id ?? "未关联";
}

function getSession(id) {
  if (!sessions.has(id)) {
    sessions.set(id, { openCard: null });
    addTab(id);
  }
  return sessions.get(id);
}

function addTab(id) {
  const tab = document.createElement("span");
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

function applyFilter() {
  for (const el of mainEl.querySelectorAll(".entry")) {
    el.hidden = activeSession !== "all" && el.dataset.session !== activeSession;
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
  appendRaw(msg);
  const p = msg.payload;
  if (p.kind === "turn_requested") {
    const card = newCard(msg, "live", "sending");
    card.head.main.textContent = "请求已发送，等待建立…";
    const userText = lastUserText(p.request);
    if (userText) {
      const el = document.createElement("div");
      el.className = "part part-user";
      el.textContent = userText;
      card.body.appendChild(el);
    }
    const details = document.createElement("details");
    const summary = document.createElement("summary");
    const count = p.request.conversation?.messages?.length ?? 0;
    summary.textContent = `请求快照 · ${count} 条消息`;
    const pre = document.createElement("pre");
    pre.className = "snapshot";
    pre.textContent = JSON.stringify(p.request, null, 2);
    details.append(summary, pre);
    card.body.appendChild(details);
    getSession(sessionOf(msg)).openCard = card;
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
      getSession(sessionOf(msg)).openCard = null;
      break;
    }
    case "TurnFailed": {
      const card = ensureCard(msg);
      card.el.classList.add("failed");
      card.badge.textContent = "失败";
      card.badge.className = "badge err";
      card.usage.textContent = fmtError(d.error);
      getSession(sessionOf(msg)).openCard = null;
      break;
    }
  }
}

function lastUserText(request) {
  const messages = request.conversation?.messages ?? [];
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message.role === "user") {
      return message.turn.parts.map((part) => part.data?.text ?? "").join("\n");
    }
  }
  return "";
}

function newCard(msg, badgeClass, badgeText) {
  const el = document.createElement("div");
  el.className = "turn entry";
  el.dataset.session = sessionOf(msg);
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
  const session = getSession(sessionOf(msg));
  if (!session.openCard) session.openCard = newCard(msg, "live", "streaming");
  return session.openCard;
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
  const card = getSession(sessionOf(msg)).openCard;
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
  el.textContent = JSON.stringify(msg);
  appendEntry(rawEl, el);
  while (rawEl.childElementCount > RAW_CAP) rawEl.firstChild.remove();
}

function appendEntry(panel, el) {
  el.hidden = activeSession !== "all" && el.dataset.session !== activeSession;
  panel.appendChild(el);
  if (!el.hidden && panel.scrollHeight - panel.scrollTop - panel.clientHeight < 120) {
    panel.scrollTop = panel.scrollHeight;
  }
}
