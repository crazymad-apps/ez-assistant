import type {
  RunStatus,
  SessionId,
  TokenUsageSnapshot,
  ToolActivityStatus,
  ToolCallId,
} from "../generated/assistant-protocol";

const MAX_TOOL_OUTPUT_CHARS = 4_000;

export type LiveReasoningSegment = Readonly<{
  type: "reasoning";
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
  created_at_ms: number;
  status: RunStatus;
  active_step: number;
  steps: readonly LiveExecutionStep[];
  usage: TokenUsageSnapshot | null;
  error_message: string | null;
}>;

export function emptyRun(
  session_id: SessionId,
  run_id: string,
  created_at_ms: number,
): LiveRunProjection {
  return {
    session_id,
    run_id,
    created_at_ms,
    status: "accepted",
    active_step: 0,
    steps: [],
    usage: null,
    error_message: null,
  };
}

export function runKey(session_id: SessionId, run_id: string): string {
  return `${session_id}:${run_id}`;
}

export function ensureStep(
  steps: readonly LiveExecutionStep[],
  step: number,
): readonly LiveExecutionStep[] {
  return steps.some((item) => item.step === step) ? steps : [...steps, { step, segments: [] }];
}

export function updateCurrentStep(
  run: LiveRunProjection,
  update: (segments: readonly LiveExecutionSegment[]) => readonly LiveExecutionSegment[],
): LiveRunProjection {
  const active_step = run.active_step || 1;
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
  call_id: ToolCallId,
  patch: Partial<LiveToolSnapshot>,
): LiveRunProjection {
  return {
    ...run,
    steps: run.steps.map((step) => ({ ...step, segments: updateTool(step.segments, call_id, patch) })),
  };
}

export function updateRunToolOutput(
  run: LiveRunProjection,
  call_id: ToolCallId,
  channel: "stdout" | "stderr",
  chunk: string,
): LiveRunProjection {
  return {
    ...run,
    steps: run.steps.map((step) => ({
      ...step,
      segments: updateToolOutput(step.segments, call_id, channel, chunk),
    })),
  };
}

export function appendTextDelta(
  segments: readonly LiveExecutionSegment[],
  type: "reasoning" | "text",
  part_id: string,
  delta: string,
): readonly LiveExecutionSegment[] {
  const index = segments.findIndex((segment) => segment.type === type && segment.part_id === part_id);
  if (index < 0) {
    return [...segments, { type, part_id, text: delta }];
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
