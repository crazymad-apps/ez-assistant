import { request as httpRequest } from 'node:http';
import { request as httpsRequest } from 'node:https';
import type { IncomingHttpHeaders, IncomingMessage } from 'node:http';
import type { Protocol } from '../contracts/models.js';

export type TransferSink = {
  headers(status: number, headers: IncomingHttpHeaders): void;
  chunk(chunk: Buffer, signal: AbortSignal): Promise<void>;
};

export class TransferError extends Error {
  constructor(
    readonly reason: 'connect_timeout' | 'upstream_idle' | 'downstream_idle' | 'cancelled' | 'upstream_connection',
  ) {
    super(reason);
  }
}

export function upstreamUrl(endpoint: string, protocol: Protocol): URL {
  const url = new URL(endpoint);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new TransferError('upstream_connection');
  }
  url.pathname = `${url.pathname.replace(/\/$/, '')}/${protocol === 'open_ai_responses' ? 'responses' : 'chat/completions'}`;
  return url;
}

/** 仅白名单响应头出站；保留压缩语义，但绝不透传中心控制头、Cookie、Location。 */
export function responseHeaders(headers: IncomingHttpHeaders): Record<string, string> {
  const result: Record<string, string> = {};
  for (const name of ['content-type', 'content-encoding', 'retry-after', 'x-request-id']) {
    const value = headers[name];
    if (typeof value === 'string' && value.length <= 1024 && !/[\r\n]/.test(value)) result[name] = value;
  }
  return result;
}

/** 单次传输不重试、不跟随跳转、不解压。背压等待有独立期限，不计入上游空闲。 */
export async function transfer(input: {
  endpoint: string;
  protocol: Protocol;
  apiKey: string;
  body: Buffer;
  signal: AbortSignal;
  connectTimeout: number;
  idleTimeout: number;
  sink: TransferSink;
  sent?: () => void;
}): Promise<number> {
  input.signal.throwIfAborted();
  const url = upstreamUrl(input.endpoint, input.protocol);
  const abort = new AbortController();
  let failure: TransferError | undefined;
  let response: IncomingMessage | undefined;
  let timer: NodeJS.Timeout | undefined;

  const fail = (reason: TransferError['reason']) => {
    if (failure) return;
    failure = new TransferError(reason);
    abort.abort(failure);
    response?.destroy(failure);
  };

  const arm = (reason: TransferError['reason'], delay: number) => {
    clearTimeout(timer);
    timer = setTimeout(() => fail(reason), delay);
    timer.unref();
  };

  const cancelled = () => fail('cancelled');

  input.signal.addEventListener('abort', cancelled, { once: true });
  if (input.signal.aborted) cancelled();
  const request = (url.protocol === 'https:' ? httpsRequest : httpRequest)(url, {
    method: 'POST',
    agent: false,
    signal: abort.signal,
    headers: {
      'content-type': 'application/json',
      accept: 'application/json, text/event-stream',
      'accept-encoding': 'identity',
      'content-length': input.body.length,
      ...(input.apiKey ? { authorization: `Bearer ${input.apiKey}` } : {}),
    },
  });
  arm('connect_timeout', input.connectTimeout);
  request.once('socket', (socket) => {
    socket.once(url.protocol === 'https:' ? 'secureConnect' : 'connect', () => arm('upstream_idle', input.idleTimeout));
  });
  try {
    response = await new Promise<IncomingMessage>((resolve, reject) => {
      request.once('error', reject);
      request.once('response', resolve);
      request.end(input.body, () => {
        input.body = Buffer.alloc(0);
        input.sent?.();
      });
    });
    const status = response.statusCode ?? 502;
    input.sink.headers(status, response.headers);
    arm('upstream_idle', input.idleTimeout);
    for await (const data of response) {
      const chunk = Buffer.isBuffer(data) ? data : Buffer.from(data);
      arm('downstream_idle', input.idleTimeout);
      await input.sink.chunk(chunk, abort.signal);
      abort.signal.throwIfAborted();
      arm('upstream_idle', input.idleTimeout);
    }
    if (!response.complete) throw new TransferError('upstream_connection');
    return status;
  } catch {
    throw failure ?? new TransferError('upstream_connection');
  } finally {
    clearTimeout(timer);
    input.signal.removeEventListener('abort', cancelled);
    response?.destroy();
    request.destroy();
  }
}
