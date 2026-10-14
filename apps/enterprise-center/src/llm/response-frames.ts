import type { IncomingHttpHeaders } from 'node:http';

export type ResponseFrame = {
  mode: 'json' | 'sse';
  value?: unknown;
  done?: boolean;
  event?: string;
  error?: string;
  usageRelevant?: boolean;
};

const eventLimit = 1024 * 1024;
const jsonLimit = 8 * eventLimit;
const sharedLimit = 16 * eventLimit;
let buffered = 0;

/** usage 与响应观察共用单次解析。预算只计持有的字节，帧交付后立即释放；不影响转发。 */
export class ResponseFrames {
  private mode: 'json' | 'sse' | 'unsupported' = 'unsupported';
  private chunks: Buffer[] = [];
  private size = 0;
  private reserved = 0;
  private lineBytes = 0;
  private skipLf = false;
  private discarded = false;
  private first = true;
  private finished = false;
  reason: string | null = null;

  constructor(private readonly consume: (frame: ResponseFrame) => void) {}

  headers(headers: IncomingHttpHeaders): void {
    const encoding = headers['content-encoding'];
    const type = headers['content-type'];
    if (encoding && String(encoding).toLowerCase() !== 'identity') this.reason = 'unsupported_encoding';
    else if (typeof type === 'string' && /^text\/event-stream(?:\s*;|$)/i.test(type)) this.mode = 'sse';
    else if (typeof type === 'string' && /^application\/(?:[a-z0-9.+-]+\+)?json(?:\s*;|$)/i.test(type))
      this.mode = 'json';
    else this.reason = 'unsupported_content';
  }

  private append(bytes: Buffer): void {
    if (!bytes.length || this.discarded) return;
    const limit = this.mode === 'json' ? jsonLimit : eventLimit;
    if (this.size + bytes.length > limit) {
      this.reason ??= 'parser_limit';
      this.discarded = true;
      this.release();
      return;
    }
    let offset = 0;
    while (offset < bytes.length) {
      if (this.size === this.reserved) {
        const capacity = Math.min(16384, limit - this.reserved);
        if (buffered + capacity > sharedLimit) {
          this.reason ??= 'parser_budget';
          this.discarded = true;
          this.release();
          return;
        }
        this.chunks.push(Buffer.allocUnsafe(capacity));
        this.reserved += capacity;
        buffered += capacity;
      }
      const tail = this.chunks.at(-1)!;
      const used = tail.length - (this.reserved - this.size);
      const count = Math.min(bytes.length - offset, tail.length - used);
      bytes.copy(tail, used, offset, offset + count);
      this.size += count;
      offset += count;
    }
  }

  private frame(): void {
    if (!this.discarded) {
      let text: string;
      try {
        text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(Buffer.concat(this.chunks, this.size));
      } catch {
        this.consume({ mode: this.mode as 'sse' | 'json', error: 'invalid_utf8', usageRelevant: true });
        this.release();
        return;
      }
      if (this.first) text = text.replace(/^\uFEFF/, '');
      this.first = false;
      let data = text;
      let event: string | undefined;
      if (this.mode === 'sse') {
        const values: string[] = [];
        for (const line of text.split(/\r\n|\r|\n/)) {
          const colon = line.indexOf(':');
          const key = colon < 0 ? line : line.slice(0, colon);
          const value = colon < 0 ? '' : line.slice(colon + 1).replace(/^ /, '');
          if (key === 'data') values.push(value);
          if (key === 'event') event = value;
        }
        data = values.join('\n');
        if (!values.length) {
          this.release();
          return;
        }
      }
      if (this.mode === 'sse' && data === '[DONE]') this.consume({ mode: 'sse', done: true, event });
      else {
        try {
          this.consume({ mode: this.mode as 'sse' | 'json', value: JSON.parse(data), event });
        } catch {
          this.consume({
            mode: this.mode as 'sse' | 'json',
            error: 'invalid_format',
            event,
            usageRelevant:
              this.mode === 'json' ||
              data.includes('"usage"') ||
              /^response\.(completed|failed|incomplete|cancelled)$/.test(event ?? ''),
          });
        }
      }
    }
    this.release();
    this.discarded = false;
  }

  chunk(chunk: Buffer): void {
    if (this.finished || this.mode === 'unsupported') return;
    if (this.mode === 'json') return this.append(chunk);
    // 只在真正的空行结束一帧；超限后也保留换行状态，不能把半个坏帧重新当成事件。
    let start = 0;
    for (let i = 0; i < chunk.length; i++) {
      const byte = chunk[i]!;
      if (this.skipLf) {
        this.skipLf = false;
        if (byte === 10) {
          start = i + 1;
          continue;
        }
      }
      if (byte !== 10 && byte !== 13) {
        this.lineBytes++;
        continue;
      }
      this.append(chunk.subarray(start, i + 1));
      start = i + 1;
      if (!this.lineBytes) this.frame();
      this.lineBytes = 0;
      this.skipLf = byte === 13;
    }
    this.append(chunk.subarray(start));
  }

  finish(): void {
    if (this.finished) return;
    this.finished = true;
    if (this.mode === 'json' && !this.discarded) this.frame();
    else if (this.mode === 'sse' && (this.size || this.discarded || this.lineBytes)) this.reason ??= 'incomplete_frame';
    this.release();
  }

  release(): void {
    buffered -= this.reserved;
    this.reserved = 0;
    this.size = 0;
    this.chunks = [];
  }
}
