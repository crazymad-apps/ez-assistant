import { randomUUID } from 'node:crypto';
import WebSocket from 'ws';
import type { RawData } from 'ws';
import { ControlError } from './errors.js';
import { TransferError, upstreamUrl } from './transport.js';
import type { TransferSink } from './transport.js';

export type WebsocketLimits = {
  idleMs?: number;
  retainedIdleConnections?: number;
  maxConnections?: number;
  maxPerUser?: number;
  maxBufferBytes?: number;
};
type Chain = {
  socket: WebSocket;
  user: number;
  binding: string;
  provider: string;
  id?: string;
  upstreamId?: string;
  busy: boolean;
  idleSince?: number;
};
type Input = {
  user: number;
  binding: string;
  provider: string;
  endpoint: string;
  apiKey: string;
  body: Buffer;
  signal: AbortSignal;
  connectTimeout: number;
  idleTimeout: number;
  sink: TransferSink;
  failureFrame?: (chunk: Buffer) => void;
  started?: () => void;
  sent?: () => void;
  finish?: (signal: AbortSignal) => Promise<void>;
};

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export class WebsocketResponseError extends Error {
  constructor(
    readonly status: number,
    readonly body: { error: Record<string, unknown> },
  ) {
    super('upstream_response');
  }
}

/** 只持有易失传输资源，不保存消息历史。全量请求各自建链，不按用户/模型合并会话。 */
export class ResponsesWebsocket {
  private readonly chains = new Set<Chain>();
  private readonly responses = new Map<string, Chain>();
  private closed = false;
  private idleTimer?: NodeJS.Timeout;

  constructor(private readonly limits: WebsocketLimits = {}) {}

  close(): void {
    this.closed = true;
    clearTimeout(this.idleTimer);
    for (const chain of this.chains) this.discard(chain);
  }

  revokeUser(user: number | null): void {
    for (const chain of this.chains) if (user === null || chain.user === user) this.discard(chain);
  }

  revokeProvider(provider: string): void {
    for (const chain of this.chains) if (chain.provider === provider) this.discard(chain);
  }

  private discard(chain: Chain): void {
    if (chain.id) this.responses.delete(chain.id);
    this.chains.delete(chain);
    chain.socket.terminate();
    this.scheduleIdleCollection();
  }

  /** 软上限内保留空闲链；超过后只回收已超时的最久空闲链，不触碰在途连接。 */
  private scheduleIdleCollection(): void {
    clearTimeout(this.idleTimer);
    this.idleTimer = undefined;
    if (this.closed) return;
    const idle = [...this.chains]
      .filter((chain) => !chain.busy && chain.idleSince !== undefined)
      .sort((a, b) => a.idleSince! - b.idleSince!);
    if (idle.length <= (this.limits.retainedIdleConnections ?? 20)) return;
    const oldest = idle[0]!;
    const delay = Math.max(0, oldest.idleSince! + (this.limits.idleMs ?? 10 * 60 * 1000) - Date.now());
    this.idleTimer = setTimeout(() => this.discard(oldest), delay);
    this.idleTimer.unref();
  }

  private reserve(user: number): void {
    const owned = [...this.chains].filter((chain) => chain.user === user);
    const userFull = owned.length >= (this.limits.maxPerUser ?? 32);
    const full = this.chains.size >= (this.limits.maxConnections ?? 256);
    if (!userFull && !full) return;
    // 只回收空闲链；正在生成或建连的链不会被新请求驱逐。
    const candidate = (userFull ? owned : [...this.chains])
      .filter((chain) => !chain.busy)
      .sort((a, b) => (a.idleSince ?? 0) - (b.idleSince ?? 0))[0];
    if (!candidate) throw new ControlError('llm_proxy_unavailable');
    this.discard(candidate);
  }

  private async open(input: Input): Promise<Chain> {
    this.reserve(input.user);
    const url = upstreamUrl(input.endpoint, 'open_ai_responses');
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const socket = new WebSocket(url, {
      headers: input.apiKey ? { authorization: `Bearer ${input.apiKey}` } : {},
      followRedirects: false,
      perMessageDeflate: false,
      maxPayload: this.limits.maxBufferBytes ?? 16 * 1024 * 1024,
      handshakeTimeout: input.connectTimeout,
    });
    const chain: Chain = { socket, user: input.user, binding: input.binding, provider: input.provider, busy: true };
    this.chains.add(chain);
    // 空闲时断线立即撤销映射；连接错误不改变持久能力配置。
    socket.on('error', () => this.discard(chain));
    socket.on('close', () => this.discard(chain));
    socket.on('message', () => {
      if (!chain.busy) this.discard(chain);
    });
    try {
      await new Promise<void>((resolve, reject) => {
        const abort = () => {
          cleanup();
          reject(new TransferError('cancelled'));
        };
        const opened = () => {
          cleanup();
          resolve();
        };
        const failed = () => {
          cleanup();
          reject(new TransferError('upstream_connection'));
        };
        const rejected = (_request: unknown, response: { statusCode?: number; destroy(): void }) => {
          cleanup();
          response.destroy();
          // 仅明确的升级/实现拒绝报告不支持，不从普通 400/404/认证错误猜测能力。
          reject(
            response.statusCode === 426 || response.statusCode === 501
              ? new ControlError('llm_websocket_unsupported')
              : new TransferError('upstream_connection'),
          );
        };
        const cleanup = () => {
          input.signal.removeEventListener('abort', abort);
          socket.off('open', opened).off('error', failed).off('close', failed).off('unexpected-response', rejected);
        };
        input.signal.addEventListener('abort', abort, { once: true });
        socket.once('open', opened).once('error', failed).once('close', failed).once('unexpected-response', rejected);
        if (input.signal.aborted) {
          cleanup();
          abort();
        }
      });
      return chain;
    } catch (error) {
      this.discard(chain);
      throw error;
    }
  }

  async transfer(input: Input): Promise<number> {
    input.signal.throwIfAborted();
    if (this.closed) throw new ControlError('llm_proxy_unavailable');
    const body = object(JSON.parse(input.body.toString('utf8')));
    if (
      !body ||
      (body.previous_response_id != null && body.store !== true) ||
      body.stream !== true ||
      typeof body.store !== 'boolean' ||
      !Array.isArray(body.input) ||
      Object.hasOwn(body, 'type') ||
      Object.hasOwn(body, 'generate') ||
      Object.hasOwn(body, 'conversation')
    )
      throw new ControlError('llm_request_invalid');
    const retain = body.store === true;
    const previous = typeof body.previous_response_id === 'string' ? body.previous_response_id : undefined;
    let chain: Chain;
    if (previous) {
      const found = this.responses.get(previous);
      // 主体与配置先校验，不通过映射暴露其他用户或其他模型的响应存在性。
      if (
        !found ||
        found.user !== input.user ||
        found.binding !== input.binding ||
        found.socket.readyState !== WebSocket.OPEN
      )
        throw new ControlError('previous_response_not_found');
      if (found.busy) throw new ControlError('llm_history_busy');
      chain = found;
      chain.busy = true;
      chain.idleSince = undefined;
      this.scheduleIdleCollection();
    } else chain = await this.open(input);
    try {
      input.signal.throwIfAborted();
      body.type = 'response.create';
      body.store = false;
      delete body.stream;
      if (previous) body.previous_response_id = chain.upstreamId;
      else delete body.previous_response_id;
      const wire = Buffer.from(JSON.stringify(body));
      input.body = Buffer.alloc(0);
      const result = await this.exchange(chain, input, wire, previous);
      // 完整 SSE 交付也是提交门禁；finish 失败不能保留服务端已推进的链。
      await input.finish?.(input.signal);
      input.signal.throwIfAborted();
      if (chain.socket.readyState !== WebSocket.OPEN || !this.chains.has(chain)) {
        this.discard(chain);
        return 200;
      }
      if (chain.id) this.responses.delete(chain.id);
      chain.id = result.id;
      chain.upstreamId = result.upstreamId;
      if (!retain) {
        this.discard(chain);
        return 200;
      }
      this.responses.set(result.id, chain);
      chain.busy = false;
      chain.idleSince = Date.now();
      this.scheduleIdleCollection();
      return 200;
    } catch (error) {
      this.discard(chain);
      throw error;
    }
  }

  /** 显式压缩独占新连接；完整终态转换成功后才向 HTTP 调用者发布窗口。 */
  async compact(input: Input): Promise<number> {
    input.signal.throwIfAborted();
    if (this.closed) throw new ControlError('llm_proxy_unavailable');
    const body = object(JSON.parse(input.body.toString('utf8')));
    if (
      !body ||
      !Array.isArray(body.input) ||
      typeof body.model !== 'string' ||
      Object.keys(body).some((key) => !['model', 'input', 'instructions'].includes(key))
    )
      throw new ControlError('llm_request_invalid');
    const retained = body.input.filter((item) => {
      const message = object(item);
      return message && ['user', 'developer', 'system'].includes(String(message.role));
    });
    const wire = Buffer.from(
      JSON.stringify({
        type: 'response.create',
        model: body.model,
        store: false,
        ...(body.instructions !== undefined ? { instructions: body.instructions } : {}),
        tools: [],
        include: ['reasoning.encrypted_content'],
        input: [...body.input, { type: 'compaction_trigger' }],
      }),
    );
    const chain = await this.open(input);
    try {
      await this.exchange(chain, input, wire, undefined, (response) => {
        const output = response.output;
        if (
          !Array.isArray(output) ||
          output.length !== 1 ||
          object(output[0])?.type !== 'compaction' ||
          typeof object(output[0])?.encrypted_content !== 'string' ||
          !object(output[0])?.encrypted_content ||
          !Number.isSafeInteger(response.created_at)
        )
          throw new TransferError('upstream_connection');
        const result = Buffer.from(
          JSON.stringify({
            id: response.id,
            object: 'response.compaction',
            created_at: response.created_at,
            output: [...retained, ...output],
            ...(response.usage !== undefined ? { usage: response.usage } : {}),
          }),
        );
        return result;
      });
      await input.finish?.(input.signal);
      input.signal.throwIfAborted();
      return 200;
    } finally {
      this.discard(chain);
    }
  }

  /** 单请求串行排空帧；缓冲与背压有界，取消/超时可打断正在等待的下游写入。 */
  private exchange(
    chain: Chain,
    input: Input,
    wire: Buffer,
    previous?: string,
    compact?: (response: Record<string, unknown>) => Buffer,
  ): Promise<{ id: string; upstreamId: string }> {
    return new Promise((resolve, reject) => {
      const id = `resp_ez_${randomUUID().replaceAll('-', '')}`;
      const queue: Buffer[] = [];
      const abort = new AbortController();
      let queued = 0;
      let processing = false;
      let done = false;
      let headersSent = false;
      let currentFrame: Buffer | undefined;
      let frameDelivered = false;
      let upstreamId: string | undefined;
      let timer: NodeJS.Timeout | undefined;
      const socket = chain.socket;
      const cleanup = () => {
        clearTimeout(timer);
        input.signal.removeEventListener('abort', cancelled);
        socket.off('message', message).off('error', failed).off('close', failed);
        queue.length = 0;
      };
      const fail = (error: Error) => {
        if (done) return;
        done = true;
        cleanup();
        abort.abort(error);
        reject(error);
      };
      const arm = (reason: TransferError['reason']) => {
        clearTimeout(timer);
        timer = setTimeout(() => fail(new TransferError(reason)), input.idleTimeout);
        timer.unref();
      };
      const failed = () => fail(new TransferError('upstream_connection'));
      const cancelled = () => fail(new TransferError('cancelled'));
      const drain = async () => {
        if (processing || done) return;
        processing = true;
        try {
          while (queue.length && !done) {
            const chunk = queue.shift()!;
            currentFrame = chunk;
            frameDelivered = false;
            const event = object(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(chunk)));
            if (!event || typeof event.type !== 'string' || !/^[a-z][a-z0-9_.]*$/.test(event.type))
              throw new TransferError('upstream_connection');
            const error = object(event.error);
            if (event.type === 'error') {
              if (
                !headersSent &&
                error?.code === 'previous_response_not_found' &&
                error.type === 'invalid_request_error' &&
                error.param === 'previous_response_id' &&
                previous
              )
                throw new ControlError('previous_response_not_found');
              if (!headersSent && error) {
                const status =
                  typeof event.status === 'number' &&
                  Number.isInteger(event.status) &&
                  event.status >= 400 &&
                  event.status <= 599
                    ? event.status
                    : 502;
                throw new WebsocketResponseError(status, { error });
              }
              throw new TransferError('upstream_connection');
            }
            const response = object(event.response);
            if (response) {
              if (typeof response.id !== 'string' || (upstreamId && response.id !== upstreamId))
                throw new TransferError('upstream_connection');
              upstreamId = response.id;
              if (!compact) response.id = id;
              if (Object.hasOwn(response, 'previous_response_id')) response.previous_response_id = previous ?? null;
            }
            if (Object.hasOwn(event, 'response_id')) {
              if (typeof event.response_id !== 'string' || (upstreamId && event.response_id !== upstreamId))
                throw new TransferError('upstream_connection');
              upstreamId ??= event.response_id;
              if (!compact) event.response_id = id;
            }
            if (compact) {
              if (event.type === 'response.completed') {
                if (response?.status !== 'completed' || !upstreamId || queue.length)
                  throw new TransferError('upstream_connection');
                const result = compact(response);
                input.sink.headers(200, { 'content-type': 'application/json' });
                headersSent = true;
                arm('downstream_idle');
                frameDelivered = true;
                await input.sink.chunk(result, abort.signal);
              }
            } else {
              if (!headersSent) {
                input.sink.headers(200, { 'content-type': 'text/event-stream' });
                headersSent = true;
              }
              arm('downstream_idle');
              frameDelivered = true;
              await input.sink.chunk(
                Buffer.from(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`),
                abort.signal,
              );
            }
            if (done) return;
            queued -= chunk.length;
            if (event.type === 'response.completed') {
              if (response?.status !== 'completed' || !upstreamId || queue.length)
                throw new TransferError('upstream_connection');
              done = true;
              cleanup();
              socket.resume();
              resolve({ id, upstreamId });
              return;
            }
            if (
              event.type === 'response.failed' ||
              event.type === 'response.incomplete' ||
              event.type === 'response.cancelled'
            )
              throw new TransferError('upstream_connection');
            if (queued < 64 * 1024) socket.resume();
            arm('upstream_idle');
          }
        } catch (error) {
          if (!done && currentFrame && !frameDelivered) input.failureFrame?.(currentFrame);
          fail(
            error instanceof ControlError || error instanceof TransferError || error instanceof WebsocketResponseError
              ? error
              : new TransferError('upstream_connection'),
          );
        } finally {
          processing = false;
        }
      };
      const message = (raw: RawData, binary: boolean) => {
        if (done) return;
        const chunk = Array.isArray(raw) ? Buffer.concat(raw) : Buffer.from(raw as ArrayBuffer);
        if (binary) {
          input.failureFrame?.(chunk);
          return failed();
        }
        queued += chunk.length;
        if (queued > (this.limits.maxBufferBytes ?? 16 * 1024 * 1024)) {
          input.failureFrame?.(chunk);
          return failed();
        }
        queue.push(chunk);
        if (queued >= 64 * 1024) socket.pause();
        void drain();
      };
      socket.on('message', message).once('error', failed).once('close', failed);
      input.signal.addEventListener('abort', cancelled, { once: true });
      if (input.signal.aborted) return cancelled();
      arm('upstream_idle');
      input.started?.();
      socket.send(wire, { binary: false }, (error) => {
        if (error) failed();
        else input.sent?.();
      });
    });
  }
}
