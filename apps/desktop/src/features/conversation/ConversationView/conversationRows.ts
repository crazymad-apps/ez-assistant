import type {
  AssistantMessageSnapshot,
  AssistantSegment,
  ConversationItem,
  MessageId,
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
    case "delegation":
    case "unavailable":
      return null;
  }
}

export function humanizeToolName(name: string): string {
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
