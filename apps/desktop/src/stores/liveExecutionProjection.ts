import type {
  ReasoningSource,
  ModelFailureKind,
  ModelRequestStatus,
  McpToolIdentity,
  RuntimeErrorCode,
  RunStatus,
  SessionId,
  TokenUsageSnapshot,
  ToolActivityStatus,
  ToolCallId,
  ToolInputProjection,
} from "@ez-assistant/protocol";

const MAX_TOOL_OUTPUT_CHARS = 4_000;

export type LiveReasoningSegment = Readonly<{
  type: "reasoning";
  source: ReasoningSource;
  part_id: string;
  text: string;
}>;

export type LiveTextSegment = Readonly<{
  type: "text";
  part_id: string;
  text: string;
}>;

export type LiveToolSnapshot = Readonly<{
  call_id: ToolCallId;
  tool_name: string;
  mcp_identity?: McpToolIdentity;
  input: ToolInputProjection;
  status: ToolActivityStatus;
  stdout: string;
  stderr: string;
}>;

export type LiveToolGroupSegment = Readonly<{
  type: "tool_group";
  group_id: string;
  tools: readonly LiveToolSnapshot[];
}>;

export type LiveExecutionSegment = LiveReasoningSegment | LiveTextSegment | LiveToolGroupSegment;

export type LiveExecutionStep = Readonly<{
  step: number;
  segments: readonly LiveExecutionSegment[];
}>;

export type LiveRunProjection = Readonly<{
  session_id: SessionId;
  run_id: string;
  created_at_ms: number | null;
  finished_at_ms: number | null;
  status: RunStatus;
  active_step: number;
  steps: readonly LiveExecutionStep[];
  usage: TokenUsageSnapshot | null;
  error_code: RuntimeErrorCode | null;
  error_message: string | null;
  model_failure_kind: ModelFailureKind | null;
  model_request: ModelRequestStatus | null;
}>;

export function emptyRun(
  session_id: SessionId,
  run_id: string,
  created_at_ms: number | null,
): LiveRunProjection {
  return {
    session_id,
    run_id,
    created_at_ms,
    finished_at_ms: null,
    status: "accepted",
    active_step: 0,
    steps: [],
    usage: null,
    error_code: null,
    error_message: null,
    model_failure_kind: null,
    model_request: null,
  };
}

export function runKey(session_id: SessionId, run_id: string): string {
  return `${session_id}:${run_id}`;
}

export function ensureStep(
  steps: readonly LiveExecutionStep[],
  step: number,
): readonly LiveExecutionStep[] {
  return steps.some((item) => item.step === step)
    ? steps
    : [...steps, { step, segments: [] }].sort((left, right) => left.step - right.step);
}

/** A repeated start discards the interrupted response while preserving settled tools. */
export function restartStep(
  steps: readonly LiveExecutionStep[],
  step: number,
): readonly LiveExecutionStep[] {
  return ensureStep(steps, step).map((item) => item.step === step
    ? { ...item, segments: item.segments.filter((segment) => segment.type === "tool_group") }
    : item);
}

export function updateStep(
  run: LiveRunProjection,
  active_step: number,
  update: (segments: readonly LiveExecutionSegment[]) => readonly LiveExecutionSegment[],
): LiveRunProjection {
  return {
    ...run,
    active_step,
    steps: ensureStep(run.steps, active_step).map((step) =>
      step.step === active_step ? { ...step, segments: update(step.segments) } : step,
    ),
  };
}

export function updateRunTools(
  run: LiveRunProjection,
  step: number,
  call_id: ToolCallId,
  patch: Partial<LiveToolSnapshot>,
): LiveRunProjection {
  return {
    ...run,
    steps: run.steps.map((item) => item.step === step
      ? { ...item, segments: updateTool(item.segments, call_id, patch) }
      : item),
  };
}

export function updateRunToolOutput(
  run: LiveRunProjection,
  step: number,
  call_id: ToolCallId,
  channel: "stdout" | "stderr",
  chunk: string,
): LiveRunProjection {
  return {
    ...run,
    steps: run.steps.map((item) => item.step === step
      ? { ...item, segments: updateToolOutput(item.segments, call_id, channel, chunk) }
      : item),
  };
}

export function appendTextDelta(
  segments: readonly LiveExecutionSegment[],
  type: "reasoning" | "text",
  part_id: string,
  delta: string,
  source: ReasoningSource = "raw",
): readonly LiveExecutionSegment[] {
  const index = segments.findIndex((segment) => segment.type === type && segment.part_id === part_id);
  if (index < 0) {
    return [...segments, type === "reasoning"
      ? { type, part_id, text: delta, source }
      : { type, part_id, text: delta }];
  }
  return segments.map((segment, item_index) =>
    item_index === index && segment.type !== "tool_group"
      ? { ...segment, text: `${segment.text}${delta}` }
      : segment,
  );
}

export function appendTool(
  segments: readonly LiveExecutionSegment[],
  tool: LiveToolSnapshot,
): readonly LiveExecutionSegment[] {
  const existing = findTool(segments, tool.call_id);
  if (existing) {
    return segments;
  }
  const last = segments.at(-1);
  if (last?.type === "tool_group") {
    return [...segments.slice(0, -1), { ...last, tools: [...last.tools, tool] }];
  }
  return [...segments, { type: "tool_group", group_id: `live-tools:${tool.call_id}`, tools: [tool] }];
}

export function reconcileTools(
  segments: readonly LiveExecutionSegment[],
  tools: readonly LiveToolSnapshot[],
): readonly LiveExecutionSegment[] {
  let next = segments;
  for (const tool of tools) {
    next = findTool(next, tool.call_id) ? updateTool(next, tool.call_id, tool) : appendTool(next, tool);
  }
  return next;
}

export function removeCommittedTools(
  segments: readonly LiveExecutionSegment[],
  committed_call_ids: ReadonlySet<ToolCallId>,
): readonly LiveExecutionSegment[] {
  const next: LiveExecutionSegment[] = [];
  for (const segment of segments) {
    if (segment.type !== "tool_group") {
      next.push(segment);
      continue;
    }
    const tools = segment.tools.filter((tool) => !committed_call_ids.has(tool.call_id));
    if (tools.length > 0) {
      next.push({ ...segment, tools });
    }
  }
  return next;
}

function updateTool(
  segments: readonly LiveExecutionSegment[],
  call_id: ToolCallId,
  patch: Partial<LiveToolSnapshot>,
): readonly LiveExecutionSegment[] {
  return segments.map((segment) =>
    segment.type === "tool_group"
      ? {
          ...segment,
          tools: segment.tools.map((tool) => tool.call_id === call_id ? { ...tool, ...patch } : tool),
        }
      : segment,
  );
}

function updateToolOutput(
  segments: readonly LiveExecutionSegment[],
  call_id: ToolCallId,
  channel: "stdout" | "stderr",
  chunk: string,
): readonly LiveExecutionSegment[] {
  const tool = findTool(segments, call_id);
  if (!tool) {
    return segments;
  }
  return updateTool(segments, call_id, {
    [channel]: `${tool[channel]}${chunk}`.slice(-MAX_TOOL_OUTPUT_CHARS),
  });
}

function findTool(
  segments: readonly LiveExecutionSegment[],
  call_id: ToolCallId,
): LiveToolSnapshot | undefined {
  for (const segment of segments) {
    if (segment.type === "tool_group") {
      const tool = segment.tools.find((item) => item.call_id === call_id);
      if (tool) {
        return tool;
      }
    }
  }
  return undefined;
}

/** Host 墙钟与本地单调时钟的接收锚点，只属于当前连接的显示投影。 */
export type HostClockAnchor = Readonly<{ host_at_ms: number; received_at_ms: number }>;

export function hostClockAnchor(host_at_ms: number | null | undefined, received_at_ms: number): HostClockAnchor | null {
  return validHostTime(host_at_ms) && Number.isFinite(received_at_ms) && received_at_ms >= 0
    ? { host_at_ms, received_at_ms }
    : null;
}

/** 终态只读可靠起止；活动值基于 Host 锚点估计，绝不相减两台机器的墙钟。 */
export function executionElapsedMs(
  run: Pick<LiveRunProjection, "created_at_ms" | "finished_at_ms" | "status">,
  anchor: HostClockAnchor | null,
  monotonic_now_ms: number,
): number | null {
  if (!validHostTime(run.created_at_ms)) return null;
  if (run.status !== "accepted" && run.status !== "running" && run.status !== "cancelling") {
    return validHostTime(run.finished_at_ms) && run.finished_at_ms >= run.created_at_ms
      ? run.finished_at_ms - run.created_at_ms
      : null;
  }
  if (!anchor || !Number.isFinite(monotonic_now_ms) || monotonic_now_ms < anchor.received_at_ms
    || anchor.host_at_ms < run.created_at_ms) return null;
  const elapsed = anchor.host_at_ms - run.created_at_ms + monotonic_now_ms - anchor.received_at_ms;
  return Number.isSafeInteger(Math.floor(elapsed)) ? elapsed : null;
}

function validHostTime(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}
