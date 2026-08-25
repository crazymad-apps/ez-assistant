import type {
  AssistantMessageSnapshot,
  AssistantSegment,
  ConversationItem,
  MessageId,
  ModelFailureKind,
  RuntimeErrorCode,
  ToolActivityStatus,
  ToolInputSnapshot,
  UserMessageSnapshot,
} from "../../../generated/assistant-protocol";
import type {
  LiveExecutionSegment,
  LiveRunProjection,
  LiveToolSnapshot,
} from "../../../stores/LiveExecutionStore";

export type ConversationRow =
  | { type: "user"; message: UserMessageSnapshot }
  | { type: "assistant_turn"; key: string; run_id: string | null; messages: AssistantMessageSnapshot[] };

export function groupConversationTurns(
  items: readonly ConversationItem[],
  group_unowned_assistant_messages = false,
): ConversationRow[] {
  const rows: ConversationRow[] = [];
  for (const item of items) {
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
        key: `assistant:${item.run_id ?? item.message_id}`,
        run_id: item.run_id,
        messages: [item],
      });
    }
  }
  return rows;
}

export function segmentStateKey(
  message_id: MessageId,
  segment: AssistantSegment,
  index: number,
): string {
  if (segment.type === "tool_group") {
    return `${message_id}:tools:${segment.tools.map((tool) => tool.call_id).join(":")}:${index}`;
  }
  return `${message_id}:${segment.type}:${segment.part_id}:${index}`;
}

export function liveSegmentStateKey(
  run_id: string,
  step: number,
  segment: LiveExecutionSegment,
  index: number,
): string {
  return segment.type === "tool_group"
    ? `${run_id}:${step}:${segment.group_id}:${index}`
    : `${run_id}:${step}:${segment.type}:${segment.part_id}:${index}`;
}

export function collapsedSummary(segments: readonly AssistantSegment[]): string {
  const reasoning = segments.filter((segment) => segment.type === "reasoning").length;
  const tools = segments.flatMap((segment) => segment.type === "tool_group" ? segment.tools : []).length;
  return [reasoning ? `${reasoning} 段思考` : "", tools ? `${tools} 个工具` : ""]
    .filter(Boolean)
    .join(" · ") || "助手回复";
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
    case "shell":
      return input.command;
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

export function runFailureMessage(
  kind: ModelFailureKind | null,
  code: RuntimeErrorCode | null,
): string {
  const kind_message: Partial<Record<ModelFailureKind, string>> = {
    configuration: "当前模型配置无法用于此会话，请检查配置或切换模型后重试。",
    authentication: "模型认证失败，请检查 API Key 和访问权限。",
    connection: "无法连接模型服务，请检查网络和 Endpoint。",
    timeout: "模型响应超时，请稍后重试。",
    stream_interrupted: "模型响应意外中断，请重试本轮。",
    provider_rejected: "模型服务拒绝了本次请求，请检查模型能力与请求参数。",
    rate_limited: "模型服务请求过于频繁，请稍后重试。",
    service_unavailable: "模型服务暂时不可用，请稍后重试。",
    context_overflow: "当前会话超出模型上下文窗口，请整理上下文后重试。",
    protocol: "模型服务返回了无法识别的响应。",
    tool_arguments: "模型生成的工具参数无效，请重试本轮。",
    cancelled: "本轮已取消。",
  };
  if (kind) {
    return kind_message[kind] ?? "本轮执行失败，请重试。";
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
