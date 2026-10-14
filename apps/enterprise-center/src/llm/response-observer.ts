import { createHash } from 'node:crypto';
import type { IncomingHttpHeaders } from 'node:http';
import type { CallEntity, Outcome } from './entity.js';
import { ResponseFrames } from './response-frames.js';
import type { ResponseFrame } from './response-frames.js';
import type { UsageObserver } from './usage.js';

export type Observation = 'pending' | 'normal' | 'anomalous' | 'unconfirmed';
export type ObservationResult = { state: Observation; reason: string | null };
const object = (value: unknown): Record<string, unknown> | null =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null;

/** 只判断可观察的协议终态，不复刻 Adapter 校验，也不把 usage 缺失当成响应失败。 */
export class ResponseObserver {
  readonly frames: ResponseFrames;
  private anomaly: string | null = null;
  private uncertain: string | null = null;
  private terminal: string | null = null;
  private terminalDigest: string | null = null;
  private responseId: string | null = null;
  private done = false;
  private choices = new Map<number, string | null>();

  constructor(
    private readonly row: Pick<CallEntity, 'protocol' | 'kind'>,
    readonly usage?: UsageObserver,
  ) {
    this.frames = new ResponseFrames((frame) => {
      this.usage?.accept(frame);
      this.accept(frame);
    });
  }

  headers(headers: IncomingHttpHeaders): void {
    this.frames.headers(headers);
  }

  chunk(chunk: Buffer): void {
    this.frames.chunk(chunk);
  }

  private identity(id: unknown): void {
    if (id === undefined) return;
    if (typeof id !== 'string' || !id || id.length > 1024) {
      this.anomaly ??= 'invalid_response_identity';
      return;
    }
    if (this.responseId !== null && this.responseId !== id) this.anomaly ??= 'response_identity_conflict';
    this.responseId ??= id;
  }

  private end(status: string): void {
    if (this.terminal && this.terminal !== status) this.anomaly ??= 'terminal_conflict';
    this.terminal ??= status;
    if (status !== 'completed') this.anomaly ??= `response_${status}`;
  }

  private accept(frame: ResponseFrame): void {
    if (frame.error) {
      this.anomaly ??= frame.error;
      return;
    }
    if (frame.done) {
      this.done = true;
      return;
    }
    const value = object(frame.value);
    if (!value) {
      this.anomaly ??= 'invalid_response_structure';
      return;
    }
    if (value.error != null || value.type === 'error') {
      this.anomaly ??= 'response_error';
      return;
    }
    if (this.row.kind === 'input_tokens') {
      if (frame.mode === 'json' && Number.isSafeInteger(value.input_tokens) && Number(value.input_tokens) >= 0)
        this.end('completed');
      else this.uncertain ??= 'unexpected_measurement';
      return;
    }
    if (this.row.kind === 'context_compaction' && frame.mode === 'json') {
      if (
        value.object === 'response.compaction' &&
        typeof value.id === 'string' &&
        Array.isArray(value.output) &&
        value.output.some(
          (item) =>
            object(item)?.type === 'compaction' &&
            typeof object(item)?.encrypted_content === 'string' &&
            object(item)?.encrypted_content,
        )
      )
        this.end('completed');
      else this.uncertain ??= 'unexpected_compaction';
      return;
    }
    if (this.row.protocol === 'open_ai_chat_completions') {
      this.identity(value.id);
      if (!Array.isArray(value.choices)) {
        this.anomaly ??= 'invalid_choices';
        return;
      }
      if (this.done && value.choices.length) this.anomaly ??= 'content_after_terminal';
      for (const item of value.choices) {
        const choice = object(item);
        if (!choice || !Number.isSafeInteger(choice.index) || Number(choice.index) < 0) {
          this.anomaly ??= 'invalid_choice';
          continue;
        }
        const index = Number(choice.index);
        if (!this.choices.has(index) && this.choices.size >= 1024) {
          this.uncertain ??= 'parser_limit';
          continue;
        }
        const previous = this.choices.get(index);
        const finish = choice.finish_reason;
        if (finish != null && typeof finish !== 'string') {
          this.anomaly ??= 'invalid_finish_reason';
          continue;
        }
        if (previous && finish != null && previous !== finish) this.anomaly ??= 'terminal_conflict';
        if (previous && object(choice.delta) && Object.keys(object(choice.delta)!).length)
          this.anomaly ??= 'content_after_terminal';
        this.choices.set(index, typeof finish === 'string' ? finish : (previous ?? null));
        if (finish === 'length' || finish === 'content_filter') this.anomaly ??= `finish_${finish}`;
        else if (finish != null && !['stop', 'tool_calls', 'function_call'].includes(String(finish)))
          this.uncertain ??= 'unknown_finish_reason';
        if (frame.mode === 'json' && !object(choice.message)) this.anomaly ??= 'invalid_message';
      }
      if (frame.mode === 'json') this.done = true;
      return;
    }
    this.identity(value.response_id);
    const response = frame.mode === 'json' ? value : object(value.response);
    this.identity(response?.id);
    if (frame.mode === 'sse') {
      if (typeof value.type !== 'string' || (frame.event && frame.event !== value.type)) {
        this.anomaly ??= 'event_type_conflict';
        return;
      }
      if (this.terminal && /\.(delta|added)$/.test(value.type)) this.anomaly ??= 'content_after_terminal';
      if (!['response.completed', 'response.failed', 'response.incomplete', 'response.cancelled'].includes(value.type))
        return;
      const expected = value.type.slice('response.'.length);
      if (!response || response.status !== expected) this.anomaly ??= 'terminal_conflict';
    }
    if (!response) {
      this.anomaly ??= 'invalid_response_structure';
      return;
    }
    const status = response.status;
    if (typeof status === 'string' && ['completed', 'failed', 'incomplete', 'cancelled'].includes(status)) {
      if (typeof response.id !== 'string' || !Array.isArray(response.output))
        this.anomaly ??= 'invalid_response_structure';
      if (response.error != null) this.anomaly ??= 'response_error';
      const digest = createHash('sha256')
        .update(JSON.stringify({ id: response.id, status, output: response.output }))
        .digest('hex');
      if (this.terminalDigest && this.terminalDigest !== digest) this.anomaly ??= 'terminal_conflict';
      this.terminalDigest ??= digest;
      this.end(status);
    } else this.uncertain ??= 'missing_terminal';
  }

  result(outcome: Outcome, reason: string | null): ObservationResult {
    this.frames.finish();
    if (outcome !== 'forwarded')
      return { state: 'anomalous', reason: reason ?? (outcome === 'upstream_error' ? 'http_error' : outcome) };
    if (this.anomaly) return { state: 'anomalous', reason: this.anomaly };
    const uncertainty = this.frames.reason ?? this.uncertain;
    if (uncertainty) return { state: 'unconfirmed', reason: uncertainty };
    const normal =
      this.row.protocol === 'open_ai_chat_completions'
        ? this.done && this.choices.size > 0 && [...this.choices.values()].every((value) => value !== null)
        : this.terminal === 'completed';
    return { state: normal ? 'normal' : 'unconfirmed', reason: normal ? null : 'missing_terminal' };
  }

  release(): void {
    this.frames.release();
    this.usage?.release();
  }
}
