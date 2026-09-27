import type { IncomingHttpHeaders } from 'node:http';
import type { Protocol } from '../contracts/models.js';
import type { UsageState } from './entity.js';

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
const fields: UsageField[] = ['input_tokens', 'output_tokens', 'total_tokens', 'cached_input_tokens', 'reasoning_tokens'];
const eventLimit = 1024 * 1024;
const jsonLimit = 8 * 1024 * 1024;
const sharedLimit = 16 * 1024 * 1024;
let buffered = 0;

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
  const detailsOut = object(source[protocol === 'open_ai_responses' ? 'output_tokens_details' : 'completion_tokens_details']);
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
  private mode: 'json' | 'sse' | 'unsupported' = 'unsupported';
  private buffer = Buffer.alloc(0);
  private counts: Counts | null = null;
  private reason: string | null = null;
  private terminal = false;
  private discardedEvent = false;
  private responseId: string | null = null;

  constructor(private readonly protocol: Protocol, private readonly providerType: string | null) {}

  headers(headers: IncomingHttpHeaders): void {
    if (headers['content-encoding'] && headers['content-encoding'] !== 'identity') {
      this.reason = 'unsupported_encoding';
      return;
    }
    const type = headers['content-type'];
    if (typeof type !== 'string') {
      this.reason = 'unsupported_content';
      return;
    }
    if (/^text\/event-stream(?:;|$)/i.test(type)) this.mode = 'sse';
    else if (/^application\/json(?:;|$)/i.test(type)) this.mode = 'json';
    else this.reason = 'unsupported_content';
  }

  private reserve(chunk: Buffer, limit: number): boolean {
    if (this.buffer.length + chunk.length > limit || buffered + chunk.length > sharedLimit) {
      this.reason = this.buffer.length + chunk.length > limit ? 'parser_limit' : 'parser_budget';
      return false;
    }
    buffered += chunk.length;
    this.buffer = Buffer.concat([this.buffer, chunk]);
    return true;
  }

  private consume(bytes: number): void {
    buffered -= bytes;
    this.buffer = this.buffer.subarray(bytes);
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
    if (this.counts && fields.some((field) => parsed[field] !== null && this.counts![field] !== null && BigInt(parsed[field]!) < BigInt(this.counts![field]!)))
      this.reason = 'invalid_usage';
    this.counts = parsed;
    const { input_tokens: input, output_tokens: output, total_tokens: total, cached_input_tokens: cached, reasoning_tokens: reasoning } = parsed;
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

  private event(bytes: Buffer): void {
    const lines = bytes.toString('utf8').split(/\r?\n/);
    const data = lines.filter((line) => line.startsWith('data:')).map((line) => line.slice(5).trimStart()).join('\n');
    if (data === '[DONE]') {
      this.terminal = true;
      return;
    }
    if (!data) return;
    try {
      const payload = object(JSON.parse(data));
      if (!payload) return;
      if (this.protocol === 'open_ai_chat_completions') {
        this.trackResponse(payload.id);
        if (payload.usage !== undefined && payload.usage !== null) this.observe(payload.usage);
      } else {
        this.trackResponse(payload.response_id);
        this.trackResponse(object(payload.response)?.id);
        if (!['response.completed', 'response.incomplete', 'response.failed', 'response.cancelled'].includes(String(payload.type))) return;
        this.terminal = true;
        const response = object(payload.response);
        if (response?.usage !== undefined && response.usage !== null) this.observe(response.usage);
      }
    } catch {
      if (data.includes('"usage"') || lines.some((line) => /^event:\s*response\.(completed|incomplete|failed|cancelled)/.test(line)))
        this.reason = 'invalid_usage';
    }
  }

  chunk(chunk: Buffer): void {
    if (this.mode === 'unsupported' || this.reason === 'parser_budget' || (this.mode === 'json' && this.reason === 'parser_limit')) return;
    if (this.mode === 'json') {
      this.reserve(chunk, jsonLimit);
      return;
    }
    for (let offset = 0; offset < chunk.length; offset += 1024) {
      if (!this.reserve(chunk.subarray(offset, offset + 1024), eventLimit)) {
        this.discardedEvent = true;
        this.release();
        continue;
      }
      for (;;) {
        const lf = this.buffer.indexOf('\n\n');
        const crlf = this.buffer.indexOf('\r\n\r\n');
        if (lf < 0 && crlf < 0) break;
        const bytes = crlf >= 0 && (lf < 0 || crlf < lf) ? crlf + 4 : lf + 2;
        if (!this.discardedEvent) this.event(this.buffer.subarray(0, bytes));
        this.consume(bytes);
        this.discardedEvent = false;
      }
    }
  }

  result(outcome: string): UsageResult {
    if (this.mode === 'json' && this.buffer.length && this.reason !== 'parser_limit') {
      try {
        const body = object(JSON.parse(this.buffer.toString('utf8')));
        if (body?.usage !== undefined && body.usage !== null) this.observe(body.usage);
        this.terminal = outcome === 'forwarded' || outcome === 'upstream_error';
      } catch {
        this.reason = 'invalid_usage';
      }
    }
    const counts = this.counts ?? empty();
    const hasValue = fields.some((field) => counts[field] !== null);
    let reason = this.reason;
    if (!reason && !this.terminal) reason = outcome === 'rejected' ? 'not_forwarded' : outcome === 'interrupted' ? 'interrupted' : 'not_reported';
    if (!reason && !hasValue) reason = 'not_reported';
    const complete = counts.input_tokens !== null && counts.output_tokens !== null && counts.total_tokens !== null;
    if (!reason && hasValue && !complete) reason = 'incomplete_usage';
    const state: UsageState = !hasValue ? 'unavailable' : !reason && complete ? 'available' : 'partial';
    this.release();
    return { ...counts, state, reason };
  }

  release(): void {
    buffered -= this.buffer.length;
    this.buffer = Buffer.alloc(0);
  }
}
