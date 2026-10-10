import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocketServer } from 'ws';
import Fastify from 'fastify';
import { expect, it, vi } from 'vitest';
import { IdentityService } from '../../dist/identity/service.js';
import { ModelsService } from '../../dist/models/service.js';
import { ModelTemplates } from '../../dist/models/templates.js';
import { emptyParameters } from '../../dist/models/parameters.js';
import { ModelProxy } from '../../dist/llm/proxy.js';
import { CallRecords, CallRecording } from '../../dist/llm/records.js';
import { CallEntity } from '../../dist/llm/entity.js';
import { SnapshotFiles, SnapshotMemory } from '../../dist/llm/snapshots.js';
import { ControlError } from '../../dist/llm/errors.js';

/** 仅身份/配置/索引使用夹具；正式 Host 二进制、Center HTTP 代理和 WS 传输均真实执行。
 * 每次新建 Runtime Home，不连接 PostgreSQL，也不复用任何用户或手工测试数据。
 */
it.skipIf(process.env.CENTER_WS_HOST_TEST !== '1')(
  '正式 Host 不改协议即可经 Center 接续，断线前置 400 自动全量重建一次',
  async () => {
    const root = await mkdtemp(resolve('../../.runtime-test/f06-ws-host-'));
    const home = join(root, 'host');
    await mkdir(home);
    const user = {
      id: 1,
      username: 'probe',
      display_name: 'probe',
      role: 'user' as const,
      is_super_admin: false,
      enabled: true,
    };
    const center_id = randomUUID(),
      providerId = randomUUID(),
      binding = 'b'.repeat(64);
    const identity = { center_id, user };
    const credentials = { ...identity, token: `ct_${'a'.repeat(64)}`, llm_key: `cl_${'a'.repeat(64)}` };
    const parameters = {
      ...emptyParameters(),
      context_window_tokens: { state: 'known' as const, value: 128000 },
      max_output_tokens: { state: 'known' as const, value: 1024 },
      streaming: 'supported' as const,
      image_input: 'unsupported' as const,
      tool_calls: 'supported' as const,
      reasoning: 'unsupported' as const,
      tool_choice: {
        auto: 'supported' as const,
        none: 'supported' as const,
        required: 'supported' as const,
        named: 'supported' as const,
      },
      reasoning_mode: 'unsupported' as const,
      tool_image_projection: 'unsupported' as const,
    };
    const textSse = await readFile('../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse', 'utf8');
    const upstream = createServer((_request, response) => {
      response.setHeader('content-type', 'text/event-stream');
      response.end(textSse);
    });
    await new Promise<void>((done) => upstream.listen(0, '127.0.0.1', done));
    const addr = upstream.address();
    if (!addr || typeof addr === 'string') throw new Error('missing upstream port');
    const ws = new WebSocketServer({ server: upstream });
    const wire: { connection: number; body: Record<string, unknown> }[] = [];
    let serial = 0;
    ws.on('connection', (socket) => {
      const connection = ++serial;
      socket.on('message', (raw) => {
        wire.push({ connection, body: JSON.parse(raw.toString()) });
        // Codex WS 在模型事件之前发送附加元数据，Host 应统一忽略未知事件。
        socket.send(JSON.stringify({ type: 'codex.rate_limits', rate_limits: { allowed: true } }));
        socket.send(JSON.stringify({ type: 'codex.response.metadata', headers: { 'x-codex-turn-state': 'fixture' } }));
        for (const line of textSse.split('\n').filter((line) => line.startsWith('data: {'))) {
          const event = JSON.parse(line.slice(6).replaceAll('resp_text_1', `resp_up_${wire.length}`)) as {
            response?: { store: boolean };
          };
          if (event.response) event.response.store = false;
          socket.send(JSON.stringify(event));
        }
      });
    });
    const identities = new IdentityService();
    vi.spyOn(identities, 'llmIdentity').mockImplementation(async (key) => {
      if (key !== credentials.llm_key) throw new ControlError('llm_key_invalid');
      return identity;
    });
    const models = new ModelsService(identities, undefined, new ModelTemplates());
    vi.spyOn(models, 'admit').mockImplementation(async (_key, _selection, _protocol, _test, history) => {
      if (history?.request.store && history.binding !== binding)
        throw new ControlError('managed_model_configuration_stale');
      return {
        provider: {
          provider_instance_id: providerId,
          api_key: '',
          server_history_key: 'test',
          connection: {
            display_name: 'probe',
            provider_type: 'openai',
            endpoint: `http://127.0.0.1:${addr.port}/v1`,
            protocol_preference: 'responses',
            models_path: '',
            discovery_format: 'openai',
            server_history_enabled: true,
            server_history_support_override: 'supported',
            responses_transport: 'websocket',
          },
          model_catalog: null,
          catalog_refreshed_at: null,
          catalog_connection_changed: false,
        },
        configuration: {
          provider_instance_id: providerId,
          model_id: 'm',
          origin: 'manual',
          source: 'fixed',
          parameters,
          field_sources: {},
          updated_at_ms: null,
          template_document: null,
          template_checked_on: null,
          requires_configuration: false,
        },
      };
    });
    const records = new CallRecords(undefined, new SnapshotFiles());
    const outcomes: { status: number | null; reason: string | null }[] = [];
    vi.spyOn(records, 'begin').mockImplementation(async () => {
      const record = new CallRecording(
        Object.assign(new CallEntity(), { id: randomUUID(), started_at: new Date() }),
        records,
        new SnapshotMemory(),
        false,
      );
      vi.spyOn(record, 'settle').mockImplementation(async (_outcome, status, reason) => {
        outcomes.push({ status, reason });
      });
      return record;
    });
    const proxy = new ModelProxy(identities, models, records, {});
    const http: Record<string, unknown>[] = [];
    const execute = proxy.execute.bind(proxy);
    vi.spyOn(proxy, 'execute').mockImplementation((input) => {
      http.push(JSON.parse(input.body.toString()));
      return execute(input);
    });
    const app = Fastify();
    app.get('/api/info', async () => ({
      protocol_version: 2,
      min_protocol_version: 2,
      capabilities: ['managed_models', 'llm_proxy'],
    }));
    app.post('/api/auth/login', async () => credentials);
    app.get('/api/auth/me', async () => identity);
    app.post('/api/auth/logout', async (_request, reply) => reply.status(204).send());
    app.get('/api/runtime/agents', async () => []);
    app.get('/api/runtime/plugins', async () => []);
    let origin: string;
    app.get('/api/runtime/model-configuration', async () => ({
      state: 'ready',
      center_id,
      endpoint: `${origin}/api/llm/providers/${providerId}/v1`,
      provider_type: 'openai',
      provider_display_name: 'probe',
      protocol: 'open_ai_responses',
      model_id: 'm',
      parameters,
      server_history_enabled: true,
      server_history_support: 'supported',
      server_history_binding: binding,
    }));
    proxy.install(app);
    origin = await app.listen({ host: '127.0.0.1', port: 0 });
    const reservation = createServer();
    await new Promise<void>((done) => reservation.listen(0, '127.0.0.1', done));
    const hostAddress = reservation.address();
    if (!hostAddress || typeof hostAddress === 'string') throw new Error('missing Host port');
    await new Promise<void>((done) => reservation.close(() => done()));
    await writeFile(
      join(home, 'host.toml'),
      `version='0.27.0'\nmode='enterprise'\n[enterprise]\ncenter_url='${origin}'\n[host_access]\nport=${hostAddress.port}\nremote_enabled=false\n`,
    );
    const host = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', home], {
      stdio: ['ignore', 'ignore', 'pipe'],
    });
    let stderr = '';
    host.stderr.on('data', (chunk) => {
      stderr += String(chunk);
    });
    const headers = (token: string) => ({
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
      'x-ez-client-version': '0.29.0',
      'x-ez-min-compatible-version': '0.29.0',
    });
    let discovery!: { address: string; access_token: string };
    try {
      await expect
        .poll(
          async () => {
            if (host.exitCode !== null) throw new Error(stderr);
            try {
              discovery = JSON.parse(await readFile(join(home, 'run/runtime.json'), 'utf8'));
              return (await fetch(`${discovery.address}/health`, { headers: headers(discovery.access_token) })).ok;
            } catch {
              return false;
            }
          },
          { timeout: 15000 },
        )
        .toBe(true);
      const { authorization: _authorization, ...loginHeaders } = headers('');
      const login = await fetch(`${discovery.address}/auth/login`, {
        method: 'POST',
        headers: loginHeaders,
        body: JSON.stringify({ method: 'enterprise', username: 'probe', password: 'fixture-password1', native: true }),
      });
      expect(login.status).toBe(200);
      const token = ((await login.json()) as { token: string }).token;
      type Payload = { session: { session_id: string }; run: { run_id: string; status: string } };
      const command = async (type: string, payload: object) => {
        const response = await fetch(`${discovery.address}/commands`, {
          method: 'POST',
          headers: headers(token),
          body: JSON.stringify({ request_id: randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }),
        });
        const result = (await response.json()) as { result: { payload: { payload: Payload } } };
        expect(response.ok, JSON.stringify(result)).toBe(true);
        return result.result.payload.payload;
      };
      await command('refresh_model_source', {});
      const session_id = (
        await command('create_session', { title: 'WS bridge probe', workspace_id: null, model_selection: null })
      ).session.session_id;
      const run = async (message: string) => {
        const submitted = await command('submit_input', { session_id, message, variant: 'build' });
        await expect
          .poll(async () => (await command('get_run', { session_id, run_id: submitted.run.run_id })).run.status, {
            timeout: 15000,
          })
          .toBe('completed');
      };
      await run('WS_HOST_FIRST');
      expect(wire).toHaveLength(1);
      expect(wire[0]?.body.previous_response_id).toBeUndefined();
      await run('WS_HOST_SECOND');
      expect(wire).toHaveLength(2);
      expect(wire[1]?.connection).toBe(wire[0]?.connection);
      expect(wire[1]?.body.previous_response_id).toBe('resp_up_1');
      expect(JSON.stringify(wire[1]?.body.input)).not.toContain('WS_HOST_FIRST');
      expect(http.filter((body) => body.store === true).at(-1)?.previous_response_id).toMatch(/^resp_ez_/);
      for (const socket of ws.clients) socket.terminate();
      await expect.poll(() => ws.clients.size).toBe(0);
      const before = http.filter((body) => body.store === true).length;
      await run('WS_HOST_REBUILD');
      const restored = http.filter((body) => body.store === true).slice(before);
      expect(restored).toHaveLength(2);
      expect(restored[0]?.previous_response_id).toMatch(/^resp_ez_/);
      expect(restored[1]?.previous_response_id).toBeUndefined();
      expect(JSON.stringify(restored[1]?.input)).toContain('WS_HOST_FIRST');
      expect(wire).toHaveLength(3);
      expect(wire[2]?.connection).not.toBe(wire[1]?.connection);
      expect(outcomes).toContainEqual({ status: 400, reason: 'previous_response_not_found' });
      await run('WS_HOST_RESUMED');
      expect(wire.at(-1)?.body.previous_response_id).toBe('resp_up_3');
      console.log(`隔离 Host 验证通过：${root}；身份/配置/索引为夹具，无 PostgreSQL 连接。`);
    } finally {
      if (host.exitCode === null) {
        host.kill('SIGTERM');
        await new Promise<void>((done) => host.once('exit', () => done()));
      }
      await proxy.close();
      await app.close();
      for (const socket of ws.clients) socket.terminate();
      await new Promise<void>((done) => ws.close(() => done()));
      upstream.closeAllConnections();
      await new Promise<void>((done) => upstream.close(() => done()));
      vi.restoreAllMocks();
    }
  },
  90000,
);
