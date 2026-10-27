import type {
  AssistantMessageSnapshot,
  ConversationItem,
  ModelFailureKind,
  RuntimeErrorCode,
  ToolActivityStatus,
  ToolInputSnapshot,
  UserMessageSnapshot,
} from "@ez-assistant/protocol";
import type {
  LiveRunProjection,
  LiveToolSnapshot,
} from "../../../stores/LiveExecutionStore";

export type ConversationRow =
  | { type: "user"; message: UserMessageSnapshot }
  | { type: "context_summary"; message: Extract<ConversationItem, { type: "context_summary" }> }
  | { type: "control_result"; message: Extract<ConversationItem, { type: "control_result" | "skill_refresh_result" | "shell_switch_result" }> }
  | { type: "assistant_turn"; key: string; run_id: string | null; messages: AssistantMessageSnapshot[]; live_run?: LiveRunProjection | null };

export function groupConversationTurns(
  items: readonly ConversationItem[],
  group_unowned_assistant_messages = false,
): ConversationRow[] {
  const rows: ConversationRow[] = [];
  for (const item of items) {
    if (item.type === "control_result" || item.type === "skill_refresh_result" || item.type === "shell_switch_result") {
      rows.push({ type: "control_result", message: item });
      continue;
    }
    if (item.type === "context_summary") {
      rows.push({ type: "context_summary", message: item });
      continue;
    }
    if (item.type === "user") {
      rows.push({ type: "user", message: item });
      continue;
    }
    const previous = rows.at(-1);
    const can_join_previous = previous?.type === "assistant_turn"
      && (
        (item.run_id !== null && previous.messages.at(-1)?.run_id === item.run_id)
        || (
          group_unowned_assistant_messages
          && item.run_id === null
          && previous.messages.at(-1)?.run_id === null
        )
      );
    if (can_join_previous) {
      previous.messages.push(item);
    } else {
      rows.push({
        type: "assistant_turn",
        key: rows.some((row) => row.type === "assistant_turn" && row.run_id === item.run_id && item.run_id !== null)
          ? `assistant:${item.run_id}:${item.message_id}` : `assistant:${item.run_id ?? item.message_id}`,
        run_id: item.run_id,
        messages: [item],
      });
    }
  }
  return rows;
}

export function toolSummary(tool: LiveToolSnapshot): string | null {
  const content = tool.stderr.trim() || tool.stdout.trim();
  return content ? content.split("\n").at(-1)?.slice(0, 120) ?? null : null;
}

export function visibleToolSummary(summary?: string | null): string | null {
  const value = summary?.trim();
  if (!value || value.startsWith("{") || value.startsWith("[")) {
    return null;
  }
  return value;
}

export function toolStatusLabel(status: ToolActivityStatus): string {
  return { proposed: "等待执行", running: "执行中", completed: "完成", failed: "失败" }[status];
}

export function toolInputLabel(input: ToolInputSnapshot): string | null {
  switch (input.type) {
    case "file": {
      const segments = input.path.split("/").filter(Boolean);
      return segments.at(-1) ?? input.path;
    }
    case "files":
      return input.paths.length === 1 ? input.paths[0] ?? null : `${input.paths.length} 个路径`;
    case "shell":
      return input.command;
    case "mcp":
      return `${input.identity.server_display_name} / ${input.identity.tool_name}`;
    case "general":
      return input.summary;
    case "image_inspection":
      return input.goal;
    case "delegation":
    case "unavailable":
      return null;
  }
}

export function humanizeToolName(name: string): string {
  if (name === "load_skill") return "加载技能";
  return name.replaceAll("_", " ");
}

export function runStatusLabel(status: LiveRunProjection["status"]): string {
  return {
    accepted: "等待执行",
    running: "正在生成",
    cancelling: "正在停止",
    completed: "已完成",
    failed: "执行失败",
    cancelled: "已取消",
    interrupted: "已中断",
    compaction_required: "正在整理上下文",
  }[status];
}

/** 请求次数是当前模型调用内的次数，不是重新发送用户消息的次数。 */
export function liveRunStatusLabel(run: LiveRunProjection): string {
  const request = run.model_request;
  if (run.status === "running" && request?.type === "retry_waiting") {
    return `连接异常，等待 ${Math.ceil(request.delay_ms / 1_000)} 秒后进行第 ${request.next_attempt} 次尝试`;
  }
  if (run.status === "running" && request?.type === "requesting" && request.attempt > 1) {
    return `正在重新连接模型 · 第 ${request.attempt} 次尝试`;
  }
  return runStatusLabel(run.status);
}

export function runFailureMessage(
  kind: ModelFailureKind | null,
  code: RuntimeErrorCode | null,
  detail: string | null = null,
): string {
  const kind_message: Partial<Record<ModelFailureKind, string>> = {
    configuration: "当前模型配置无法用于此会话，请检查配置或切换模型后重试。",
    authentication: "模型认证失败，请检查 API Key 和访问权限。",
    connection: "无法连接模型服务，请检查网络和服务地址。",
    timeout: "模型响应超时，请稍后重试。",
    stream_interrupted: "模型响应意外中断，已停止自动重试。请检查已有结果后继续。",
    provider_rejected: "模型服务拒绝了本次请求，请检查模型能力与请求参数。",
    rate_limited: "模型服务请求过于频繁，请稍后重试。",
    service_unavailable: "模型服务暂时不可用，请稍后重试。",
    context_overflow: "当前会话超出模型上下文窗口，请整理上下文后重试。",
    protocol: "模型服务返回了无法识别的响应。",
    tool_arguments: "模型生成的工具参数无效，请重试本轮。",
    cancelled: "本轮已取消。",
  };
  // 只解释 Runtime 已持久化的固定脱敏摘要，不展示或解析 Provider 原始错误。
  const diagnostics = code === "model_execution_failed"
    ? /^model execution failed (before|after) stream establishment \(kind=([a-z_]+), attempts=(\d+), retries=(\d+), output_observed=(true|false)\)$/.exec(detail ?? "")
    : null;
  const reason = kind ?? diagnostics?.[2];
  const message = reason ? kind_message[reason as ModelFailureKind] : null;
  if (diagnostics?.[1] === "after") {
    return "模型响应中断，已停止自动重试。请检查已有结果后继续。";
  }
  if (message) {
    return diagnostics && Number(diagnostics[3]) > 1
      ? `已尝试 ${diagnostics[3]} 次，未能恢复。${message}`
      : message;
  }
  const code_message: Partial<Record<RuntimeErrorCode, string>> = {
    configuration_unavailable: "模型配置当前不可用，请检查配置后重试。",
    model_not_found: "当前模型不存在，请重新选择模型。",
    model_unavailable: "当前模型不可用，请检查配置或切换模型。",
    model_build_failed: "当前模型配置无法加载，请检查配置后重试。",
    model_execution_failed: "模型执行失败，请检查模型配置或稍后重试。",
    agent_build_failed: "智能体无法启动，请检查模型和会话配置。",
    timeout: "本轮执行超时，请稍后重试。",
    cancelled: "本轮已取消。",
  };
  return (code && code_message[code]) || "本轮执行失败，请重试。";
}

export function formatTime(timestamp: number | null): string {
  if (!timestamp) {
    return "";
  }
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(timestamp);
}

export function formatDateTime(timestamp: number | null): string {
  if (!timestamp) {
    return "";
  }
  const date = new Date(timestamp);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 将实时尾部并入同一行，首次提交时保持 React 身份；child 只接自己的最后一个消息段。 */
export function attachLiveTurn(rows: ConversationRow[], live: LiveRunProjection | null, child_id: string | null): ConversationRow[] {
  // 继承身份不是 live Run 的目标；旧事件或同名 ID 不能恢复为可执行尾部。
  const readonly_target = [...rows].reverse().find((row) => row.type === "assistant_turn"
    && (child_id ? row.run_id === null : row.run_id === live?.run_id));
  if (readonly_target?.type === "assistant_turn"
    && readonly_target.messages.some((message) => message.inherited || message.partial)) return rows;
  const target = [...rows].reverse().find((row) => row.type === "assistant_turn" && (child_id ? row.run_id === null : row.run_id === live?.run_id));
  if (target?.type === "assistant_turn") {
    target.live_run = live;
    if (child_id) target.key = `assistant:child:${child_id}`;
  } else if (live && (child_id !== null || live.status !== "accepted")) {
    rows.push({ type: "assistant_turn", key: child_id ? `assistant:child:${child_id}` : `assistant:${live.run_id}`, run_id: live.run_id, messages: [], live_run: live });
  }
  return rows;
}
