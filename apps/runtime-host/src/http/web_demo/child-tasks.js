(() => {
  const TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled", "interrupted"]);
  const MAX_RENDERED_MESSAGES = 240;
  const MAX_RENDERED_TEXT_CHARS = 120_000;
  const MAX_LIVE_TOOL_OUTPUT_CHARS = 12_000;

  const local = {
    sessionId: null,
    parentRunIds: [],
    tasks: new Map(),
    views: new Map(),
    parentUsage: { input_tokens: 0, output_tokens: 0, total_tokens: 0, cached_input_tokens: 0 },
    parentUsageByMessage: new Map(),
    liveParentUsage: new Map(),
    childUsageByMessage: new Map(),
    liveChildUsage: new Map(),
    frame: null,
    pendingViews: new Set(),
    pendingTools: new Set(),
  };

  let bridge = null;

  const byId = (id) => document.getElementById(id);
  const emptyUsage = () => ({
    input_tokens: 0,
    output_tokens: 0,
    total_tokens: 0,
    cached_input_tokens: 0,
  });

  function addUsage(target, usage = {}) {
    target.input_tokens += usage.input_tokens || 0;
    target.output_tokens += usage.output_tokens || 0;
    target.total_tokens += usage.total_tokens || 0;
    target.cached_input_tokens += usage.cached_input_tokens || 0;
  }

  function summedUsage(entries) {
    const total = emptyUsage();
    for (const usage of entries) addUsage(total, usage);
    return total;
  }

  function usageFromConversation(conversation) {
    const usage = new Map();
    for (const message of conversation.messages || []) {
      if ((message.role === "assistant" || message.role === "context_summary") && message.turn?.usage) {
        usage.set(message.turn.id, message.turn.usage);
      }
      if (message.role === "context_summary" && message.turn?.compacted_usage) {
        usage.set(`${message.turn.id}:compacted`, message.turn.compacted_usage);
      }
    }
    return usage;
  }

  function formatCount(value) {
    return Number.isFinite(value) ? value.toLocaleString("zh-CN") : "—";
  }

  function renderAggregateUsage() {
    const parent = summedUsage([
      ...local.parentUsageByMessage.values(),
      ...local.liveParentUsage.values(),
    ]);
    const children = summedUsage([
      ...local.childUsageByMessage.values(),
      ...local.liveChildUsage.values(),
    ]);
    const combined = emptyUsage();
    addUsage(combined, parent);
    addUsage(combined, children);
    local.parentUsage = parent;
    byId("usage-parent-total").textContent = formatCount(parent.total_tokens);
    byId("usage-child-total").textContent = formatCount(children.total_tokens);
    byId("usage-combined-total").textContent = formatCount(combined.total_tokens);
    byId("usage-combined-cached").textContent = formatCount(combined.cached_input_tokens);
  }

  function reset(sessionId, parentRunIds = []) {
    if (local.frame !== null) cancelAnimationFrame(local.frame);
    local.sessionId = sessionId;
    local.parentRunIds = [...new Set(parentRunIds)];
    local.tasks.clear();
    local.views.clear();
    local.parentUsageByMessage.clear();
    local.liveParentUsage.clear();
    local.childUsageByMessage.clear();
    local.liveChildUsage.clear();
    local.pendingViews.clear();
    local.pendingTools.clear();
    local.frame = null;
    const list = byId("child-task-list");
    list.replaceChildren();
    list.className = "child-task-list empty";
    list.textContent = sessionId ? "当前父 Run 暂无子任务" : "请选择 Session";
    renderAggregateUsage();
  }

  function setParentConversation(conversation) {
    local.parentUsageByMessage = usageFromConversation(conversation);
    local.liveParentUsage.clear();
    renderAggregateUsage();
  }

  function setParentUsage(runId, step, usage) {
    local.liveParentUsage.set(`${runId}:${step}`, usage);
    renderAggregateUsage();
  }

  function visibleText(text) {
    if (!text) return "";
    if (text.length <= MAX_RENDERED_TEXT_CHARS) return text;
    return `${text.slice(0, MAX_RENDERED_TEXT_CHARS)}\n\n[Demo 已省略其余内容]`;
  }

  function statusText(task) {
    const error = task.error ? ` · ${task.error.code}` : "";
    return `${task.status}${task.cancel_requested ? " · cancel requested" : ""}${error}`;
  }

  function createTaskView(task) {
    const article = document.createElement("article");
    article.className = "child-task-item";
    article.dataset.childTaskId = task.child_task_id;
    const summary = document.createElement("div");
    summary.className = "child-task-summary";
    const identity = document.createElement("button");
    identity.className = "child-task-toggle";
    identity.type = "button";
    const title = document.createElement("strong");
    title.textContent = task.title;
    const meta = document.createElement("small");
    meta.textContent = `${task.child_task_id} · ${task.variant} · ${statusText(task)}`;
    identity.append(title, meta);
    const actions = document.createElement("div");
    actions.className = "child-task-actions";
    const cancel = document.createElement("button");
    cancel.className = "danger";
    cancel.textContent = "取消子任务";
    cancel.disabled = TERMINAL_STATUSES.has(task.status);
    cancel.addEventListener("click", async (event) => {
      event.stopPropagation();
      cancel.disabled = true;
      try {
        const result = await bridge.runtimeCommand("cancel_child_task", {
          session_id: local.sessionId,
          child_task_id: task.child_task_id,
        });
        upsertTask(result.task);
      } catch (error) {
        bridge.setStatus(`子任务取消失败：${error.message}`, true);
        cancel.disabled = TERMINAL_STATUSES.has(local.tasks.get(task.child_task_id)?.status);
      }
    });
    actions.append(cancel);
    summary.append(identity, actions);

    const details = document.createElement("div");
    details.className = "child-task-details";
    details.hidden = true;
    const usage = document.createElement("small");
    usage.className = "child-task-usage";
    usage.textContent = "用量：—";
    const messages = document.createElement("div");
    messages.className = "child-message-list empty";
    messages.textContent = "展开后加载独立 Conversation";
    details.append(usage, messages);
    article.append(summary, details);

    const view = {
      article,
      meta,
      cancel,
      details,
      usage,
      messages,
      expanded: false,
      pendingText: [],
      pendingReasoning: [],
      liveAssistant: null,
      liveTools: new Map(),
      conversation: null,
    };
    identity.addEventListener("click", async () => {
      view.expanded = !view.expanded;
      details.hidden = !view.expanded;
      article.classList.toggle("expanded", view.expanded);
      if (view.expanded) {
        if (view.conversation) renderConversation(view, view.conversation);
        else await loadConversation(task.child_task_id, true);
      }
    });
    return view;
  }

  function upsertTask(task) {
    if (!task || task.session_id !== local.sessionId) return;
    local.tasks.set(task.child_task_id, task);
    let view = local.views.get(task.child_task_id);
    if (!view) {
      view = createTaskView(task);
      local.views.set(task.child_task_id, view);
      const list = byId("child-task-list");
      if (list.classList.contains("empty")) list.replaceChildren();
      list.classList.remove("empty");
      list.append(view.article);
    }
    view.meta.textContent = `${task.child_task_id} · ${task.variant} · ${statusText(task)}`;
    view.cancel.disabled = TERMINAL_STATUSES.has(task.status);
    view.article.dataset.status = task.status;
  }

  function renderConversation(view, conversation) {
    const childTaskId = view.article.dataset.childTaskId;
    view.pendingText.length = 0;
    view.pendingReasoning.length = 0;
    view.liveAssistant = null;
    view.liveTools.clear();
    view.conversation = conversation;
    for (const key of local.liveChildUsage.keys()) {
      if (key.startsWith(`${childTaskId}:`)) local.liveChildUsage.delete(key);
    }
    for (const key of local.childUsageByMessage.keys()) {
      if (key.startsWith(`${childTaskId}:`)) local.childUsageByMessage.delete(key);
    }
    view.messages.replaceChildren();
    view.messages.className = "child-message-list";
    const messages = conversation.messages || [];
    const visible = messages.slice(-MAX_RENDERED_MESSAGES);
    const toolNames = new Map();
    if (visible.length < messages.length) {
      const notice = document.createElement("div");
      notice.className = "history-truncation";
      notice.textContent = `已省略更早的 ${messages.length - visible.length} 条消息`;
      view.messages.append(notice);
    }
    for (const message of visible) {
      const turn = message.turn || {};
      const card = document.createElement("article");
      card.className = `child-message ${message.role}`;
      const label = document.createElement("strong");
      label.textContent = message.role === "assistant" ? "Assistant" : message.role === "tool" ? "工具" : "任务输入";
      const body = document.createElement("div");
      if (message.role === "assistant") {
        const reasoning = (turn.parts || [])
          .filter((part) => part.type === "reasoning")
          .map((part) => part.data.text).join("");
        const text = (turn.parts || [])
          .filter((part) => part.type === "text")
          .map((part) => part.data.text).join("");
        const tools = (turn.parts || [])
          .filter((part) => part.type === "tool_call")
          .map((part) => {
            toolNames.set(part.data.id, part.data.name);
            return part.data.name;
          });
        if (reasoning) {
          const reasoningNode = document.createElement("pre");
          reasoningNode.className = "child-reasoning";
          reasoningNode.textContent = visibleText(reasoning);
          body.append(reasoningNode);
        }
        const textNode = document.createElement("div");
        textNode.textContent = visibleText(text || (tools.length ? `调用工具：${tools.join("、")}` : "（无正文）"));
        body.append(textNode);
      } else if (message.role === "tool") {
        const result = turn.result || {};
        body.textContent = `${toolNames.get(result.call_id) || "未知工具"} · ${result.call_id || ""} · ${result.status || "unknown"}`;
      } else {
        body.textContent = visibleText((turn.parts || [])
          .filter((part) => part.type === "text")
          .map((part) => part.data.text).join(""));
      }
      card.append(label, body);
      view.messages.append(card);
    }
    if (!visible.length) {
      view.messages.classList.add("empty");
      view.messages.textContent = "暂无正文";
    }
    const usage = usageFromConversation(conversation);
    for (const [messageId, value] of usage) {
      local.childUsageByMessage.set(`${childTaskId}:${messageId}`, value);
    }
    renderViewUsage(view);
    renderAggregateUsage();
  }

  function storeConversationUsage(view, conversation) {
    const childTaskId = view.article.dataset.childTaskId;
    view.conversation = conversation;
    for (const key of local.liveChildUsage.keys()) {
      if (key.startsWith(`${childTaskId}:`)) local.liveChildUsage.delete(key);
    }
    for (const key of local.childUsageByMessage.keys()) {
      if (key.startsWith(`${childTaskId}:`)) local.childUsageByMessage.delete(key);
    }
    const usage = usageFromConversation(conversation);
    for (const [messageId, value] of usage) {
      local.childUsageByMessage.set(`${childTaskId}:${messageId}`, value);
    }
    renderViewUsage(view);
    renderAggregateUsage();
  }

  async function loadConversation(childTaskId, render = false) {
    const sessionId = local.sessionId;
    const view = local.views.get(childTaskId);
    if (!sessionId || !view) return;
    view.messages.className = "child-message-list empty";
    view.messages.textContent = "正在加载子任务 Conversation…";
    try {
      const conversation = await bridge.childConversation(sessionId, childTaskId);
      if (local.sessionId !== sessionId || !local.views.has(childTaskId)) return;
      if (render || view.expanded) renderConversation(view, conversation);
      else storeConversationUsage(view, conversation);
    } catch (error) {
      view.messages.textContent = `加载失败：${error.message}`;
    }
  }

  function renderViewUsage(view) {
    const childTaskId = view.article.dataset.childTaskId;
    const values = [];
    for (const [key, usage] of local.childUsageByMessage) {
      if (key.startsWith(`${childTaskId}:`)) values.push(usage);
    }
    for (const [key, usage] of local.liveChildUsage) {
      if (key.startsWith(`${childTaskId}:`)) values.push(usage);
    }
    const total = summedUsage(values);
    view.usage.textContent = `累计用量：Input ${formatCount(total.input_tokens)} · Output ${formatCount(total.output_tokens)} · Total ${formatCount(total.total_tokens)} · Cached ${formatCount(total.cached_input_tokens)}`;
  }

  function ensureLiveAssistant(view) {
    if (view.liveAssistant) return view.liveAssistant;
    if (view.messages.classList.contains("empty")) view.messages.replaceChildren();
    view.messages.classList.remove("empty");
    const card = document.createElement("article");
    card.className = "child-message assistant live";
    const label = document.createElement("strong");
    label.textContent = "Assistant · 生成中";
    const reasoning = document.createElement("pre");
    reasoning.className = "child-reasoning";
    reasoning.hidden = true;
    const text = document.createElement("div");
    card.append(label, reasoning, text);
    view.messages.append(card);
    view.liveAssistant = { card, reasoning, text };
    return view.liveAssistant;
  }

  function scheduleFrame(view = null, tool = null) {
    if (view) local.pendingViews.add(view);
    if (tool) local.pendingTools.add(tool);
    if (local.frame !== null) return;
    local.frame = requestAnimationFrame(() => {
      local.frame = null;
      for (const pending of local.pendingViews) {
        const live = ensureLiveAssistant(pending);
        if (pending.pendingReasoning.length) {
          live.reasoning.append(document.createTextNode(pending.pendingReasoning.join("")));
          live.reasoning.hidden = false;
          pending.pendingReasoning.length = 0;
        }
        if (pending.pendingText.length) {
          live.text.append(document.createTextNode(pending.pendingText.join("")));
          pending.pendingText.length = 0;
        }
      }
      local.pendingViews.clear();
      for (const pending of local.pendingTools) {
        pending.stdout = `${pending.stdout}${pending.pendingStdout}`
          .slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
        pending.stderr = `${pending.stderr}${pending.pendingStderr}`
          .slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
        pending.pendingStdout = "";
        pending.pendingStderr = "";
        const sections = [];
        if (pending.stdout) sections.push(`stdout\n${pending.stdout}`);
        if (pending.stderr) sections.push(`stderr\n${pending.stderr}`);
        pending.output.textContent = sections.join("\n\n");
        pending.output.hidden = !sections.length;
      }
      local.pendingTools.clear();
    });
  }

  function ensureLiveTool(view, callId, toolName = "未知工具") {
    let tool = view.liveTools.get(callId);
    if (tool) return tool;
    const card = document.createElement("article");
    card.className = "child-message tool live";
    const label = document.createElement("strong");
    label.textContent = toolName;
    const meta = document.createElement("small");
    meta.textContent = `${callId} · proposed`;
    const output = document.createElement("pre");
    output.className = "child-tool-output";
    output.hidden = true;
    card.append(label, meta, output);
    view.messages.append(card);
    tool = {
      meta,
      output,
      stdout: "",
      stderr: "",
      pendingStdout: "",
      pendingStderr: "",
    };
    view.liveTools.set(callId, tool);
    return tool;
  }

  function handleEvent(envelope) {
    if (envelope.type !== "child_task_event" || envelope.session_id !== local.sessionId) return false;
    const event = envelope.event || {};
    if (event.type === "created") upsertTask(event.task);
    const view = local.views.get(envelope.child_task_id);
    if (!view) {
      void refresh();
      return true;
    }
    if (event.type === "started") {
      const task = local.tasks.get(envelope.child_task_id);
      if (task) upsertTask({ ...task, status: "running" });
    } else if (event.type === "text_delta" || event.type === "reasoning_delta") {
      // 折叠任务不缓存无限正文；展开时从权威 Conversation 加载稳定部分，之后才接收实时增量。
      if (view.expanded) {
        if (event.type === "text_delta") view.pendingText.push(event.delta);
        else view.pendingReasoning.push(event.delta);
        scheduleFrame(view);
      }
    } else if (event.type === "usage_updated") {
      local.liveChildUsage.set(`${envelope.child_task_id}:${event.step}`, event.usage);
      renderViewUsage(view);
      renderAggregateUsage();
    } else if (event.type === "tool_proposed" && view.expanded) {
      ensureLiveTool(view, event.call_id, event.tool_name);
    } else if (event.type === "tool_started" && view.expanded) {
      ensureLiveTool(view, event.call_id).meta.textContent = `${event.call_id} · running`;
    } else if (event.type === "tool_output" && view.expanded) {
      const tool = ensureLiveTool(view, event.call_id);
      const field = event.channel === "stderr" ? "pendingStderr" : "pendingStdout";
      tool[field] = `${tool[field]}${event.chunk}`.slice(-MAX_LIVE_TOOL_OUTPUT_CHARS);
      scheduleFrame(null, tool);
    } else if (event.type === "tool_completed" && view.expanded) {
      ensureLiveTool(view, event.call_id).meta.textContent = `${event.call_id} · ${event.status}`;
    } else if (event.type === "finished") {
      const task = local.tasks.get(envelope.child_task_id);
      if (task) upsertTask({ ...task, status: event.status, error: event.error || null });
      void loadConversation(envelope.child_task_id, view.expanded);
    }
    return true;
  }

  async function refresh(parentRunIds = local.parentRunIds) {
    const sessionId = local.sessionId;
    if (!sessionId) return;
    local.parentRunIds = [...new Set(parentRunIds)];
    try {
      const results = await Promise.all(local.parentRunIds.map((parentRunId) => (
        bridge.runtimeCommand("list_child_tasks", {
          session_id: sessionId,
          parent_run_id: parentRunId,
        })
      )));
      if (local.sessionId !== sessionId) return;
      const tasks = results.flatMap((result) => result.tasks || []);
      const seen = new Set(tasks.map((task) => task.child_task_id));
      for (const task of tasks) upsertTask(task);
      for (const [childTaskId, view] of local.views) {
        if (!seen.has(childTaskId)) {
          view.article.remove();
          local.views.delete(childTaskId);
          local.tasks.delete(childTaskId);
        }
      }
      const list = byId("child-task-list");
      if (!tasks.length) {
        list.className = "child-task-list empty";
        list.textContent = "当前父 Run 暂无子任务";
      }
      await Promise.all(tasks.map((task) => {
        const view = local.views.get(task.child_task_id);
        return loadConversation(task.child_task_id, Boolean(view?.expanded));
      }));
    } catch (error) {
      bridge.setStatus(`子任务查询失败：${error.message}`, true);
    }
  }

  function initialize(nextBridge) {
    bridge = nextBridge;
    byId("refresh-child-tasks").addEventListener("click", () => refresh());
    return {
      reset,
      refresh,
      handleEvent,
      setParentConversation,
      setParentUsage,
    };
  }

  window.createChildTaskDemo = initialize;
})();
