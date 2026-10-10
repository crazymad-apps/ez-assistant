import { once } from 'node:events';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { WebSocketServer } from 'ws';
import type WebSocket from 'ws';
import { afterEach, expect, it } from 'vitest';
import { ResponsesWebsocket } from '../../dist/llm/websocket.js';

const owners: ResponsesWebsocket[] = [];
const servers: WebSocketServer[] = [];
const fixture = (await readFile('../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse', 'utf8'))
  .split('\n')
  .filter((line) => line.startsWith('data: {'))
  .map((line) => line.slice(6));
const body = {
  model: 'm',
  stream: true,
  store: true,
  instructions: 'unchanged',
  input: [{ role: 'user', content: 'first' }],
};

afterEach(async () => {
  for (const owner of owners.splice(0)) owner.close();
  for (const server of servers.splice(0)) {
    for (const socket of server.clients) socket.terminate();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});

async function upstream(handle: (socket: WebSocket, request: Record<string, unknown>, index: number) => void) {
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  servers.push(server);
  await once(server, 'listening');
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  const requests: { connection: number; body: Record<string, unknown> }[] = [];
  let connections = 0;
  server.on('connection', (socket, request) => {
    const connection = ++connections;
    expect(request.headers.authorization).toBe('Bearer fixture-key');
    expect(request.headers['x-ez-center-history-binding']).toBeUndefined();
    socket.on('message', (raw, binary) => {
      expect(binary).toBe(false);
      const parsed = JSON.parse(raw.toString()) as Record<string, unknown>;
      requests.push({ connection, body: parsed });
      handle(socket, parsed, requests.length);
    });
  });
  return { endpoint: `http://127.0.0.1:${address.port}/v1`, requests, server };
}

function complete(socket: WebSocket, index: number) {
  for (const event of fixture) socket.send(event.replaceAll('resp_text_1', `resp_up_${index}`));
}

function owner(options = {}) {
  const result = new ResponsesWebsocket(options);
  owners.push(result);
  return result;
}

async function call(bridge: ResponsesWebsocket, endpoint: string, patch: object = {}, options: object = {}) {
  const events: Record<string, unknown>[] = [];
  const status = await bridge.transfer({
    user: 1,
    provider: 'provider-a',
    binding: 'binding-a',
    endpoint,
    apiKey: 'fixture-key',
    body: Buffer.from(JSON.stringify({ ...body, ...patch })),
    signal: new AbortController().signal,
    connectTimeout: 1000,
    idleTimeout: 1000,
    sink: {
      headers(code) {
        expect(code).toBe(200);
      },
      async chunk(chunk) {
        events.push(JSON.parse(chunk.toString().split('\ndata: ')[1]!.trim()));
      },
    },
    ...options,
  });
  expect(status).toBe(200);
  const response = events.at(-1)?.response as Record<string, unknown>;
  return { id: response.id as string, events };
}

it('首轮全量与工具增量共用连接，响应 ID 一致映射且不改 call_id/正文', async () => {
  const up = await upstream((socket, _request, index) => complete(socket, index));
  const bridge = owner();
  const first = await call(bridge, up.endpoint);
  expect(first.id).toMatch(/^resp_ez_/);
  expect(first.events.filter((event) => event.response).map((event) => (event.response as { id: string }).id)).toEqual([
    first.id,
    first.id,
    first.id,
  ]);
  const output = [{ type: 'function_call_output', call_id: 'tool_1', output: 'result' }];
  const second = await call(bridge, up.endpoint, { previous_response_id: first.id, input: output });
  expect(second.id).not.toBe(first.id);
  expect(up.requests.map((request) => request.connection)).toEqual([1, 1]);
  expect(up.requests[1]?.body).toMatchObject({
    type: 'response.create',
    store: false,
    previous_response_id: 'resp_up_1',
    input: output,
  });
  expect(up.requests[0]?.body).not.toHaveProperty('stream');
  expect(up.requests[0]?.body).not.toHaveProperty('previous_response_id');
  await expect(call(bridge, up.endpoint, { previous_response_id: first.id })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  await expect(call(bridge, up.endpoint, { previous_response_id: second.id }, { user: 2 })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  await expect(
    call(bridge, up.endpoint, { previous_response_id: second.id }, { binding: 'binding-b' }),
  ).rejects.toMatchObject({ code: 'previous_response_not_found' });
  expect(up.requests).toHaveLength(2);
});

it('同一主体的全量请求仍各自建链；丢失 socket 后只能全量重建', async () => {
  const up = await upstream((socket, _request, index) => complete(socket, index));
  const bridge = owner();
  const [a, b] = await Promise.all([call(bridge, up.endpoint), call(bridge, up.endpoint)]);
  expect(up.requests.map((request) => request.connection)).toEqual([1, 2]);
  await call(bridge, up.endpoint, { previous_response_id: b.id });
  await call(bridge, up.endpoint, { previous_response_id: a.id });
  expect(up.requests.slice(2).map((request) => request.connection)).toEqual([2, 1]);
  bridge.close();
  const fresh = owner();
  await expect(call(fresh, up.endpoint, { previous_response_id: a.id })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  await call(fresh, up.endpoint);
  expect(up.requests.at(-1)?.body).not.toHaveProperty('previous_response_id');
});

it('仅首事件前精确旧 ID 拒绝可恢复；普通错误保持上游错误', async () => {
  const up = await upstream((socket, request, index) => {
    if (!request.previous_response_id) return complete(socket, index);
    socket.send(
      JSON.stringify({
        type: 'error',
        status: 400,
        error: {
          type: 'invalid_request_error',
          code: 'previous_response_not_found',
          param: 'previous_response_id',
        },
      }),
    );
  });
  const bridge = owner();
  const a = await call(bridge, up.endpoint);
  await expect(call(bridge, up.endpoint, { previous_response_id: a.id })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  const malformed = await upstream((socket) =>
    socket.send(
      JSON.stringify({ type: 'error', status: 429, error: { code: 'rate_limit_exceeded', message: 'try later' } }),
    ),
  );
  await expect(call(bridge, malformed.endpoint)).rejects.toMatchObject({
    status: 429,
    body: { error: { code: 'rate_limit_exceeded' } },
  });
});

it('交付失败撤销已经生成的链；忙碌拒绝不破坏正在执行的请求', async () => {
  let unblock!: () => void;
  let arrived!: () => void;
  const pending = new Promise<void>((resolve) => {
    arrived = resolve;
  });
  const up = await upstream((socket, request, index) => {
    if (!request.previous_response_id) return complete(socket, index);
    unblock = () => complete(socket, index);
    arrived();
  });
  const bridge = owner();
  const a = await call(bridge, up.endpoint);
  const running = call(bridge, up.endpoint, { previous_response_id: a.id });
  await pending;
  await expect(call(bridge, up.endpoint, { previous_response_id: a.id })).rejects.toMatchObject({
    code: 'llm_history_busy',
  });
  unblock();
  await running;
  await expect(
    call(
      bridge,
      up.endpoint,
      {},
      {
        finish: async () => {
          throw new Error('delivery failed');
        },
      },
    ),
  ).rejects.toThrow('delivery failed');
});

it('流中断与超时不伪造历史失效、不自动发送第二次生成', async () => {
  const up = await upstream((socket) => {
    socket.send(fixture[0]!);
    socket.close();
  });
  await expect(call(owner(), up.endpoint)).rejects.toMatchObject({ reason: 'upstream_connection' });
  expect(up.requests).toHaveLength(1);
  const stalled = await upstream(() => {});
  await expect(call(owner(), stalled.endpoint, {}, { idleTimeout: 20 })).rejects.toMatchObject({
    reason: 'upstream_idle',
  });
  expect(stalled.requests).toHaveLength(1);
});

it('明确升级拒绝与临时服务故障区分，握手失败不发送模型请求', async () => {
  for (const status of [426, 503]) {
    const server = createServer();
    server.on('upgrade', (_request, socket) =>
      socket.end(`HTTP/1.1 ${status} Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`),
    );
    await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
    try {
      const address = server.address();
      if (!address || typeof address === 'string') throw new Error('missing port');
      const request = call(owner(), `http://127.0.0.1:${address.port}/v1`);
      await expect(request).rejects.toMatchObject(
        status === 426 ? { code: 'llm_websocket_unsupported' } : { reason: 'upstream_connection' },
      );
    } finally {
      await new Promise<void>((done) => server.close(() => done()));
    }
  }
});

it('空闲过期与连接配额回收旧链；不会驱逐正在使用的链', async () => {
  const up = await upstream((socket, _request, index) => complete(socket, index));
  const bridge = owner({ maxConnections: 2, maxPerUser: 1 });
  const first = await call(bridge, up.endpoint);
  const other = await call(bridge, up.endpoint, {}, { user: 2 });
  await call(bridge, up.endpoint);
  await expect(call(bridge, up.endpoint, { previous_response_id: first.id })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  await call(bridge, up.endpoint, { previous_response_id: other.id }, { user: 2 });
  const expiring = owner({ idleMs: 10, retainedIdleConnections: 0 });
  const expired = await call(expiring, up.endpoint);
  await expect.poll(() => up.server.clients.size).toBe(2);
  await expect(call(expiring, up.endpoint, { previous_response_id: expired.id })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  let received!: () => void;
  const waiting = new Promise<void>((resolve) => {
    received = resolve;
  });
  const stuck = await upstream(() => received());
  const limited = owner({ maxConnections: 1 });
  const abort = new AbortController();
  const pending = call(limited, stuck.endpoint, {}, { signal: abort.signal }).catch((error: unknown) => error);
  await waiting;
  await expect(call(limited, stuck.endpoint)).rejects.toMatchObject({ code: 'llm_proxy_unavailable' });
  abort.abort();
  expect(await pending).toMatchObject({ reason: 'cancelled' });
  await expect.poll(() => stuck.server.clients.size).toBe(0);
});

it('主体和 Provider 撤销只清理相关链，其他链仍可继续', async () => {
  const up = await upstream((socket, _request, index) => complete(socket, index));
  const bridge = owner();
  const a = await call(bridge, up.endpoint);
  const b = await call(bridge, up.endpoint, {}, { user: 2 });
  const c = await call(bridge, up.endpoint, {}, { user: 2, provider: 'provider-b' });
  bridge.revokeUser(1);
  await expect(call(bridge, up.endpoint, { previous_response_id: a.id })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  bridge.revokeProvider('provider-a');
  await expect(call(bridge, up.endpoint, { previous_response_id: b.id }, { user: 2 })).rejects.toMatchObject({
    code: 'previous_response_not_found',
  });
  await call(bridge, up.endpoint, { previous_response_id: c.id }, { user: 2, provider: 'provider-b' });
  bridge.revokeUser(null);
  await expect.poll(() => up.server.clients.size).toBe(0);
});

it('慢下游写入可被取消，帧或累计缓冲超限时断开且不发布链头', async () => {
  const up = await upstream((socket, _request, index) => complete(socket, index));
  let writing!: () => void;
  const arrived = new Promise<void>((resolve) => {
    writing = resolve;
  });
  const abort = new AbortController();
  const blocked = call(
    owner(),
    up.endpoint,
    {},
    {
      signal: abort.signal,
      sink: {
        headers() {},
        chunk(_chunk: Buffer, signal: AbortSignal) {
          writing();
          return new Promise<void>((_resolve, reject) => {
            signal.addEventListener('abort', () => reject(signal.reason), { once: true });
          });
        },
      },
    },
  ).catch((error: unknown) => error);
  await arrived;
  abort.abort();
  expect(await blocked).toMatchObject({ reason: 'cancelled' });
  await expect.poll(() => up.server.clients.size).toBe(0);
  const flood = await upstream((socket) => {
    for (let i = 0; i < 10; i++) socket.send(JSON.stringify({ type: 'extension.data', value: 'x'.repeat(600) }));
  });
  await expect(
    call(
      owner({ maxBufferBytes: 1024 }),
      flood.endpoint,
      {},
      {
        sink: {
          headers() {},
          chunk(_chunk: Buffer, signal: AbortSignal) {
            return new Promise<void>((_resolve, reject) => {
              signal.addEventListener('abort', () => reject(signal.reason), { once: true });
            });
          },
        },
      },
    ),
  ).rejects.toMatchObject({ reason: 'upstream_connection' });
  const oversized = await upstream((socket) => socket.send('x'.repeat(2048)));
  await expect(call(owner({ maxBufferBytes: 1024 }), oversized.endpoint)).rejects.toMatchObject({
    reason: 'upstream_connection',
  });
});


it('软上限内不因空闲过期回收，溢出后只回收到保留数量', async () => {
  const up = await upstream((socket, _request, index) => complete(socket, index));
  const bridge = owner({ idleMs: 20, retainedIdleConnections: 1 });
  const first = await call(bridge, up.endpoint);
  // 再次续接仍应成功，即使测试推进到空闲期限之后。
  await new Promise((resolve) => setTimeout(resolve, 40));
  const continued = await call(bridge, up.endpoint, { previous_response_id: first.id });
  const second = await call(bridge, up.endpoint);
  await expect.poll(() => up.server.clients.size).toBe(1);
  await expect(call(bridge, up.endpoint, { previous_response_id: continued.id })).rejects.toMatchObject({ code: 'previous_response_not_found' });
  await call(bridge, up.endpoint, { previous_response_id: second.id });
});
