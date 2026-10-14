import type { IncomingHttpHeaders } from 'node:http';
import type { Protocol } from '../contracts/models.js';
import type { UsageState } from './entity.js';
import { ResponseFrames } from './response-frames.js';
import type { ResponseFrame } from './response-frames.js';

type UsageField = 'input_tokens' | 'output_tokens' | 'total_tokens' | 'cached_input_tokens' | 'reasoning_tokens';
export type UsageResult = { state: UsageState; reason: string | null } & Record<UsageField, string | null>;
type Counts = Record<UsageField, string | null>;

const empty = (): Counts => ({
  input_tokens: null,
  output_tokens: null,
  total_tokens: null,
  cached_input_tokens: null,
  reasoning_tokens: null,
});
const fields: UsageField[] = [
  'input_tokens',
  'output_tokens',
  'total_tokens',
  'cached_input_tokens',
  'reasoning_tokens',
];
function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function count(value: unknown): string | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? String(value) : null;
}

function usage(value: unknown, protocol: Protocol, providerType: string | null): Counts | null {
  const source = object(value);
  if (!source) return null;
  const input = protocol === 'open_ai_responses' ? 'input_tokens' : 'prompt_tokens';
  const output = protocol === 'open_ai_responses' ? 'output_tokens' : 'completion_tokens';
  const detailsIn = object(source[protocol === 'open_ai_responses' ? 'input_tokens_details' : 'prompt_tokens_details']);
  const detailsOut = object(
    source[protocol === 'open_ai_responses' ? 'output_tokens_details' : 'completion_tokens_details'],
  );
  const result: Counts = {
    input_tokens: count(source[input]),
    output_tokens: count(source[output]),
    total_tokens: count(source.total_tokens),
    cached_input_tokens: count(detailsIn?.cached_tokens),
    reasoning_tokens: count(detailsOut?.reasoning_tokens),
  };
  if (result.cached_input_tokens === null && protocol === 'open_ai_chat_completions' && providerType === 'deepseek')
    result.cached_input_tokens = count(source.prompt_cache_hit_tokens);
  return result;
}

/** 只观察上游响应；失败永不改变原始转发结果。 */
export class UsageObserver {
  private readonly frames = new ResponseFrames((frame) => this.accept(frame));
  private counts: Counts | null = null;
  private reason: string | null = null;
  private terminal = false;
  private responseId: string | null = null;

  constructor(
    private readonly protocol: Protocol,
    private readonly providerType: string | null,
  ) {}

  headers(headers: IncomingHttpHeaders): void {
    this.frames.headers(headers);
  }

  private observe(value: unknown): void {
    const parsed = usage(value, this.protocol, this.providerType);
    if (!parsed) {
      this.reason = 'invalid_usage';
      return;
    }
    const source = object(value)!;
    const inputKey = this.protocol === 'open_ai_responses' ? 'input_tokens' : 'prompt_tokens';
    const outputKey = this.protocol === 'open_ai_responses' ? 'output_tokens' : 'completion_tokens';
    if ([inputKey, outputKey, 'total_tokens'].some((key) => source[key] !== undefined && count(source[key]) === null))
      this.reason = 'invalid_usage';
    if (
      this.counts &&
      fields.some(
        (field) =>
          parsed[field] !== null &&
          this.counts![field] !== null &&
          BigInt(parsed[field]!) < BigInt(this.counts![field]!),
      )
    )
      this.reason = 'invalid_usage';
    this.counts = parsed;
    const {
      input_tokens: input,
      output_tokens: output,
      total_tokens: total,
      cached_input_tokens: cached,
      reasoning_tokens: reasoning,
    } = parsed;
    if (input !== null && output !== null && total !== null && BigInt(input) + BigInt(output) !== BigInt(total)) {
      parsed.total_tokens = null;
      this.reason = 'invalid_usage';
    }
    if (cached !== null && input !== null && BigInt(cached) > BigInt(input)) {
      parsed.cached_input_tokens = null;
      this.reason = 'invalid_usage';
    }
    if (reasoning !== null && output !== null && BigInt(reasoning) > BigInt(output)) {
      parsed.reasoning_tokens = null;
      this.reason = 'invalid_usage';
    }
  }

  private trackResponse(id: unknown): void {
    if (typeof id !== 'string' || !id) return;
    if (this.responseId && this.responseId !== id) this.reason = 'invalid_usage';
    else this.responseId = id;
  }

  /** 代理路径由响应观察 owner 喂同一帧；独立 usage 调用仍可使用 headers/chunk。 */
  accept(frame: ResponseFrame): void {
    if (frame.error) {
      if (frame.usageRelevant) this.reason = 'invalid_usage';
      return;
    }
    if (frame.done) {
      this.terminal = true;
      return;
    }
    const payload = object(frame.value);
    if (!payload) return;
    if (frame.mode === 'json') {
      if (payload.usage != null) this.observe(payload.usage);
      this.terminal = true;
    } else if (this.protocol === 'open_ai_chat_completions') {
      this.trackResponse(payload.id);
      if (payload.usage != null) this.observe(payload.usage);
    } else {
      this.trackResponse(payload.response_id);
      this.trackResponse(object(payload.response)?.id);
      if (
        !['response.completed', 'response.incomplete', 'response.failed', 'response.cancelled'].includes(
          String(payload.type),
        )
      )
        return;
      this.terminal = true;
      const response = object(payload.response);
      if (response?.usage != null) this.observe(response.usage);
    }
  }

  chunk(chunk: Buffer): void {
    this.frames.chunk(chunk);
  }

  result(outcome: string, parserReason?: string | null): UsageResult {
    this.frames.finish();
    this.reason ??= parserReason ?? this.frames.reason;
    const counts = this.counts ?? empty();
    const hasValue = fields.some((field) => counts[field] !== null);
    let reason = this.reason;
    if (!reason && !this.terminal)
      reason = outcome === 'rejected' ? 'not_forwarded' : outcome === 'interrupted' ? 'interrupted' : 'not_reported';
    if (!reason && !hasValue) reason = 'not_reported';
    const complete = counts.input_tokens !== null && counts.output_tokens !== null && counts.total_tokens !== null;
    if (!reason && hasValue && !complete) reason = 'incomplete_usage';
    const state: UsageState = !hasValue ? 'unavailable' : !reason && complete ? 'available' : 'partial';
    this.release();
    return { ...counts, state, reason };
  }

  release(): void {
    this.frames.release();
  }
}
