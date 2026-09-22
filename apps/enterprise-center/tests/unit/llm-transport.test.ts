import { createServer } from 'node:http';
import { once } from 'node:events';
import type { AddressInfo } from 'node:net';
import Fastify from 'fastify';
import { expect, it } from 'vitest';
import { RequestBodies, requestModel } from '../../dist/llm/body.js';
import { responseHeaders, transfer } from '../../dist/llm/transport.js';

it('只解析准入字段，拒绝重复转义 model、非对象、非法 UTF-8 与 stream', () => {
  expect(requestModel(Buffer.from('{"model":"m","n":9007199254740993,"nested":{"model":"x"}}'))).toEqual({
    model: 'm',
    stream: false,
  });
  for (const body of ['[]', '{"model":"a","mo\\u0064el":"b"}', '{"model":"a","stream":1}']) {
    expect(() => requestModel(Buffer.from(body))).toThrow();
  }
  expect(() => requestModel(Buffer.from([0xff]))).toThrow();
});

it('Fastify 私有 parser 保留字节且不改变管理接口小正文限制', async () => {
  const app = Fastify({ bodyLimit: 16 * 1024 });
  app.post('/management', async (request) => request.body);
  app.register((scope) => {
    new RequestBodies().install(scope);
    scope.post('/proxy', async (request) => request.body);
  });
  try {
    const payload = '{"model":"m", "n":9007199254740993,"text":"' + 'x'.repeat(20000) + '"}';
    const proxy = await app.inject({
      method: 'POST',
      url: '/proxy',
      payload,
      headers: { 'content-type': 'application/json' },
    });
    expect(proxy.statusCode).toBe(200);
    expect(proxy.body).toBe(payload);
    expect(
      (
        await app.inject({
          method: 'POST',
          url: '/management',
          payload,
          headers: { 'content-type': 'application/json' },
        })
      ).statusCode,
    ).toBe(413);
  } finally {
    await app.close();
  }
});

it('两协议保持原始请求和响应，过滤伪造控制头，不跟随跳转', async () => {
  const paths: string[] = [];
  const payload = Buffer.from('{"model":"m", "n":9007199254740993}');
  const server = createServer(async (request, response) => {
    paths.push(request.url!);
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(chunk);
    expect(Buffer.concat(chunks)).toEqual(payload);
    expect(request.headers.authorization).toBe('Bearer secret');
    expect(request.headers['accept-encoding']).toBe('identity');
    response.writeHead(307, {
      location: '/redirect',
      'x-ez-center-control': '1',
      'set-cookie': 'unsafe',
      'content-type': 'text/event-stream',
    });
    response.write('data: {"n":9007199254740993}\n\n');
    response.end('data: [DONE]\n\n');
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    for (const protocol of ['open_ai_responses', 'open_ai_chat_completions'] as const) {
      const chunks: Buffer[] = [];
      const status = await transfer({
        endpoint: `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1`,
        protocol,
        apiKey: 'secret',
        body: payload,
        signal: new AbortController().signal,
        connectTimeout: 1000,
        idleTimeout: 1000,
        sink: {
          headers: (_status, headers) =>
            expect(responseHeaders(headers)).toEqual({ 'content-type': 'text/event-stream' }),

          chunk: async (chunk) => {
            chunks.push(chunk);
          },
        },
      });
      expect(status).toBe(307);
      expect(Buffer.concat(chunks).toString()).toBe('data: {"n":9007199254740993}\n\ndata: [DONE]\n\n');
    }
    expect(paths).toEqual(['/v1/responses', '/v1/chat/completions']);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});

it('上游空闲超时、下游背压超时和取消均销毁网络；正常 request close 不打断响应', async () => {
  const server = createServer((request, response) => {
    request.resume();
    request.once('end', () => {
      if (request.url === '/idle/responses') return;
      response.writeHead(200);
      response.write('first');
      if (request.url === '/normal/responses') setTimeout(() => response.end('last'), 30);
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const endpoint = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const base = {
    protocol: 'open_ai_responses' as const,
    apiKey: '',
    body: Buffer.from('{}'),
    connectTimeout: 500,
    idleTimeout: 80,
  };
  try {
    await expect(
      transfer({
        ...base,
        endpoint: `${endpoint}/idle`,
        signal: new AbortController().signal,
        sink: { headers() {}, async chunk() {} },
      }),
    ).rejects.toMatchObject({ reason: 'upstream_idle' });
    await expect(
      transfer({
        ...base,
        endpoint: `${endpoint}/blocked`,
        body: Buffer.from('{}'),
        signal: new AbortController().signal,
        sink: {
          headers() {},

          chunk: async (_chunk, signal) =>
            new Promise<void>((_resolve, reject) =>
              signal.addEventListener('abort', () => reject(signal.reason), { once: true }),
            ),
        },
      }),
    ).rejects.toMatchObject({ reason: 'downstream_idle' });
    const chunks: Buffer[] = [];
    await transfer({
      ...base,
      endpoint: `${endpoint}/normal`,
      body: Buffer.from('{}'),
      signal: new AbortController().signal,
      sink: {
        headers() {},

        async chunk(chunk) {
          chunks.push(chunk);
        },
      },
    });
    expect(Buffer.concat(chunks).toString()).toBe('firstlast');
    await expect(
      transfer({
        ...base,
        endpoint: `${endpoint}/idle`,
        body: Buffer.from('{}'),
        signal: AbortSignal.timeout(20),
        sink: { headers() {}, async chunk() {} },
      }),
    ).rejects.toMatchObject({ reason: 'cancelled' });
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});

it('原始正文逐块限额拒绝 32 MiB 以上请求，释放后仍能接收下一次请求', async () => {
  const app = Fastify();
  app.register((scope) => {
    new RequestBodies().install(scope);
    scope.setErrorHandler((error, _request, reply) => {
      const failure = error as Error & { status?: number };
      void reply.status(failure.status ?? 500).send({ error: failure.message });
    });
    scope.post('/raw', async (request) => ({ bytes: (request.body as Buffer).length }));
  });
  try {
    const rejected = await app.inject({
      method: 'POST',
      url: '/raw',
      headers: { 'content-type': 'application/json' },
      payload: Buffer.alloc(32 * 1024 * 1024 + 1),
    });
    expect(rejected.statusCode).toBe(413);
    const accepted = await app.inject({
      method: 'POST',
      url: '/raw',
      headers: { 'content-type': 'application/json' },
      payload: '{}',
    });
    expect(accepted.json()).toEqual({ bytes: 2 });
  } finally {
    await app.close();
  }
});
