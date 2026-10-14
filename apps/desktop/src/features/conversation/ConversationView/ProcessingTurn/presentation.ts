import type { AssistantMessageSnapshot, AssistantSegment, RunStatus } from '@ez-assistant/protocol';
import type { LiveExecutionSegment, LiveRunProjection } from '../../../../stores/LiveExecutionStore';

/** 只组织显示顺序；可靠 message 身份用于引用/详情，step 身份用于流式提交前后连续展示。 */
export type TurnEntry = Readonly<{
  key: string;
  step: number;
}> &
  (
    | Readonly<{ message_id: string; segment: AssistantSegment; live: false }>
    | Readonly<{ message_id: null; segment: LiveExecutionSegment; live: true }>
  );

export function presentTurn(
  messages: readonly AssistantMessageSnapshot[],
  live: LiveRunProjection | null,
  status: RunStatus | null = live?.status ?? messages.at(-1)?.status ?? null,
) {
  let entries: TurnEntry[] = messages.flatMap((message, message_index) =>
    message.segments.map((segment) => ({
      key: `${message.step ?? message.message_id}:${segment.type}:${segment.type === 'tool_group' ? segment.tools[0]?.call_id : segment.part_id}`,
      step: message.step ?? message_index + 1,
      message_id: message.message_id,
      segment,
      live: false as const,
    })),
  );
  for (const step of live?.steps ?? []) {
    for (const segment of step.segments) {
      entries.push({
        key: `${step.step}:${segment.type}:${segment.type === 'tool_group' ? segment.tools[0]?.call_id : segment.part_id}`,
        step: step.step,
        message_id: null,
        segment,
        live: true,
      });
    }
  }
  // 空正文和思考片段不参与布局，也不隔断相邻工具分组。
  entries = entries.filter(({ segment }) => segment.type === 'tool_group' || segment.text.trim().length > 0);
  // 活动轮次仍按最新 Step 判断，不能提前把中间说明当作最终回复。
  // 终态可能由问答/交付等工具结束，没有后继纯文本消息；保留最后一个有正文的消息，
  // 即使它自身带工具，或后续消息只有工具/reasoning，也不能把已交付正文全部折掉。
  const terminal = status !== null && !isRunActive(status);
  const last = terminal
    ? [...entries].reverse().find((entry) => entry.segment.type === 'text' && entry.segment.text.trim().length > 0)
    : entries.at(-1);
  const same_message = (entry: TurnEntry) =>
    last && entry.live === last.live && (entry.live ? entry.step === last.step : entry.message_id === last.message_id);
  const last_has_tools = entries.some((entry) => same_message(entry) && entry.segment.type === 'tool_group');
  const is_reply = (entry: TurnEntry) => (terminal || !last_has_tools) && same_message(entry) && entry.segment.type === 'text';
  return { process: entries.filter((entry) => !is_reply(entry)), reply: entries.filter(is_reply) };
}

export function groupProcessEntries(entries: readonly TurnEntry[]): TurnEntry[][] {
  const groups: TurnEntry[][] = [];
  for (const entry of entries) {
    const previous = groups.at(-1);
    // 只有相邻同类 reasoning/工具合组；正文始终截断，不跨内容重排。
    if (
      entry.segment.type !== 'text' &&
      previous?.[0]?.segment.type === entry.segment.type &&
      (entry.segment.type === 'tool_group' ||
        (previous[0].step === entry.step &&
          previous[0].message_id === entry.message_id &&
          previous[0].live === entry.live))
    )
      previous.push(entry);
    else groups.push([entry]);
  }
  return groups;
}

export function isRunActive(status: LiveRunProjection['status'] | null | undefined): boolean {
  return status === 'accepted' || status === 'running' || status === 'cancelling';
}

export function formatElapsed(elapsed: number | null): string {
  if (elapsed === null) return '耗时不可用';
  const seconds = Math.floor(elapsed / 1000);
  if (seconds < 60) return `${seconds}秒`;
  const minutes = Math.floor(seconds / 60);
  return minutes < 60
    ? `${minutes}分${seconds % 60}秒`
    : `${Math.floor(minutes / 60)}时${minutes % 60}分${seconds % 60}秒`;
}
