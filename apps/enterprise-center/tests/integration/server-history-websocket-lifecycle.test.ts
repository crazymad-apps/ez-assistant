import { readFile, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { WebSocketServer } from 'ws';
import { expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createDataSource } from '../../dist/database/source.js';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { ModelsService } from '../../dist/models/service.js';
import { UsersService } from '../../dist/users/service.js';
import { CallManagement } from '../../dist/llm/management.js';
import { emptyParameters } from '../../dist/models/parameters.js';

// 只接受本轮显式新建的空库，不能复用 7320 或其他共享环境。
it.skipIf(!process.env.CENTER_WS_LIFECYCLE_SCENARIO)(
  '真实数据库及 Host 的桥接生命周期、配置 ABA 和主体撤销',
  async () => {
    const root = resolve(process.env.CENTER_WS_LIFECYCLE_SCENARIO!);
    if (!root.startsWith(resolve('../../.runtime-test/f06-ws-lifecycle-'))) throw new Error('隔离路径不符');
    const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
      root: string;
      host: string;
      port: number;
      database: string;
      role: string;
    };
    expect(scenario).toMatchObject({ root, host: '127.0.0.1', database: 'ez_center_ws_lifecycle', role: 'center_app' });
    const database = { url: `postgres://center_app@127.0.0.1:${scenario.port}/${scenario.database}` };
    const pool = createPool(database);
    try {
      expect((await pool.query('select current_database() as db,inet_server_port() as port')).rows[0]).toEqual({
        db: scenario.database,
        port: scenario.port,
      });
      expect((await pool.query("select count(*)::int as n from pg_tables where schemaname='public'")).rows[0].n).toBe(
        0,
      );
      await upgradeDatabase(pool);
    } finally {
      await pool.end();
    }
    const source = createDataSource(database);
    await source.initialize();
    const text = await readFile('../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse', 'utf8');
    const wire: { connection: number; body: Record<string, unknown>; bytes: number }[] = [];
    const upstream = createServer((_request, response) => {
      response.setHeader('content-type', 'text/event-stream');
      response.end(text);
    });
    await new Promise<void>((done) => upstream.listen(0, '127.0.0.1', done));
    const upstreamAddress = upstream.address();
    if (!upstreamAddress || typeof upstreamAddress === 'string') throw new Error('upstream address');
    const ws = new WebSocketServer({ server: upstream });
    let serial = 0;
    ws.on('connection', (socket) => {
      const connection = ++serial;
      socket.on('message', (raw) => {
        const body = JSON.parse(raw.toString());
        wire.push({ connection, body, bytes: Buffer.byteLength(raw.toString()) });
        socket.send(JSON.stringify({ type: 'codex.rate_limits', rate_limits: { allowed: true } }));
        socket.send(JSON.stringify({ type: 'codex.response.metadata', headers: { test: true } }));
        for (const line of text.split('\n').filter((line) => line.startsWith('data: {'))) {
          const event = JSON.parse(line.slice(6).replaceAll('resp_text_1', `resp_up_${wire.length}`));
          if (event.response) event.response.store = false;
          socket.send(JSON.stringify(event));
          if (JSON.stringify(body.input).includes('BLOCK') && event.type === 'response.created') break;
        }
      });
    });
    const reservation = createServer();
    await new Promise<void>((done) => reservation.listen(0, '127.0.0.1', done));
    const address = reservation.address();
    if (!address || typeof address === 'string') throw new Error('center address');
    const origin = `http://127.0.0.1:${address.port}`;
    await new Promise<void>((done) => reservation.close(() => done()));
    const open = async () => {
      await mkdir(join(root, 'snapshots'), { recursive: true });
      await mkdir(join(root, 'plugins'), { recursive: true });
      const app = await createHttpApp({
        source,
        origin,
        snapshotRoot: join(root, 'snapshots'),
        pluginRoot: join(root, 'plugins'),
      });
      await app.listen(address.port, '127.0.0.1');
      return app;
    };
    let app = await open();
    let host: ReturnType<typeof spawn> | undefined;
    try {
      let identity = app.get(IdentityService),
        models = app.get(ModelsService);
      let admin = await identity.login({ username: 'admin', password: '123456' }, randomUUID());
      const connection = {
        display_name: 'WS fixture',
        provider_type: 'openai',
        endpoint: `http://127.0.0.1:${upstreamAddress.port}/v1`,
        protocol_preference: 'responses',
        models_path: '',
        discovery_format: 'openai',
        responses_transport: 'websocket',
        server_history_enabled: true,
        server_history_support_override: 'supported',
      };
      const provider = await models.save(
        admin.token,
        undefined,
        { connection, credential: { mode: 'replace', value: 'fixture-key' } },
        randomUUID(),
      );
      const selection = { provider_instance_id: provider.provider_instance_id, model_id: 'fixture-model' };
      const parameters = {
        ...emptyParameters(),
        context_window_tokens: { state: 'known', value: 128000 },
        max_output_tokens: { state: 'known', value: 1024 },
        streaming: 'supported',
        image_input: 'unsupported',
        tool_calls: 'supported',
        reasoning: 'unsupported',
        tool_choice: { auto: 'supported', none: 'supported', required: 'supported', named: 'supported' },
        reasoning_mode: 'unsupported',
        tool_image_projection: 'unsupported',
      };
      const fixed = (next = parameters) =>
        models.saveConfiguration(admin.token, { ...selection, origin: 'manual', parameters: next }, randomUUID());
      const save = (next = connection) =>
        models.save(
          admin.token,
          provider.provider_instance_id,
          { connection: next, credential: { mode: 'unchanged' } },
          randomUUID(),
        );
      await fixed();
      await models.setDefault(admin.token, { default_model: selection }, randomUUID());
      await app
        .get(CallManagement)
        .saveSettings(
          admin.token,
          { enabled: true, content_retention_days: 7, index_retention_days: 90 },
          randomUUID(),
        );
      const config = async (login = admin) => {
        const response = await fetch(`${origin}/api/runtime/model-configuration`, {
          headers: { authorization: `Bearer ${login.token}` },
        });
        expect(response.status).toBe(200);
        return response.json() as Promise<{ endpoint: string; server_history_binding: string }>;
      };
      const send = async (
        previous?: string,
        input: unknown[] = [{ role: 'user', content: 'delta' }],
        login = admin,
        binding?: string,
      ) => {
        const current = await config(login);
        const body = {
          model: selection.model_id,
          stream: true,
          store: true,
          input,
          ...(previous ? { previous_response_id: previous } : {}),
        };
        const response = await fetch(`${current.endpoint}/responses`, {
          method: 'POST',
          headers: {
            authorization: `Bearer ${login.llm_key}`,
            'content-type': 'application/json',
            'x-ez-center-history-binding': binding ?? current.server_history_binding,
          },
          body: JSON.stringify(body),
        });
        const raw = await response.text();
        const events = raw
          .split('\n')
          .filter((line) => line.startsWith('data: {'))
          .map((line) => JSON.parse(line.slice(6)));
        return {
          status: response.status,
          error: response.status === 200 ? null : JSON.parse(raw).error,
          id: events.find((event) => event.type === 'response.completed')?.response.id as string,
          callId: response.headers.get('x-ez-call-id'),
        };
      };
      const firstInput = [{ role: 'user', content: 'history '.repeat(1000) }];
      const first = await send(undefined, firstInput);
      expect(first.status).toBe(200);
      expect(first.id).toMatch(/^resp_ez_/);
      await identity.logout(admin.token, randomUUID());
      admin = await identity.login({ username: 'admin', password: '123456' }, randomUUID());
      const second = await send(first.id);
      expect(second.status).toBe(200);
      expect(wire[1]?.connection).toBe(wire[0]?.connection);
      expect(wire[1]?.body.input).toEqual([{ role: 'user', content: 'delta' }]);
      expect(wire[1]?.bytes).toBeLessThan(wire[0]!.bytes / 10);
      await save();
      await fixed();
      const same = await send(second.id);
      expect(same.status).toBe(200);
      const callsBeforeFailure = wire.length;
      expect(() =>
        models.saveConfiguration(
          admin.token,
          {
            ...selection,
            origin: 'manual',
            parameters: { ...parameters, context_window_tokens: { state: 'known', value: -1 } },
          },
          randomUUID(),
        ),
      ).toThrow();
      const failedAudit = vi.spyOn(identity, 'audit').mockRejectedValueOnce(new Error('fixture rollback'));
      await expect(save({ ...connection, endpoint: 'http://127.0.0.1:1/v1' })).rejects.toThrow('fixture rollback');
      failedAudit.mockRestore();
      const afterRejectedSave = await send(same.id);
      expect(afterRejectedSave.status).toBe(200);
      expect(wire.length).toBe(callsBeforeFailure + 1);
      // 同用户不同业务链隔离；固定配置 A→B→A 立即关闭旧链，旧绑定不能误复活。
      await send();
      expect(ws.clients.size).toBe(2);
      const oldBinding = (await config()).server_history_binding;
      await fixed({ ...parameters, max_output_tokens: { state: 'known', value: 2048 } });
      await expect.poll(() => ws.clients.size).toBe(0);
      await fixed();
      expect((await config()).server_history_binding).not.toBe(oldBinding);
      expect((await send(afterRejectedSave.id, undefined, admin, oldBinding)).status).toBe(409);
      expect((await send(afterRejectedSave.id)).error.code).toBe('previous_response_not_found');
      const rebuilt = await send(undefined, firstInput);
      expect(rebuilt.status).toBe(200);
      // 变更提交时中止在途请求，且不重发。
      const beforeBlocked = wire.length;
      const blocked = send(rebuilt.id, [{ role: 'user', content: 'BLOCK' }]).catch(() => 'interrupted');
      await expect.poll(() => wire.length).toBe(beforeBlocked + 1);
      await save({ ...connection, server_history_enabled: false });
      expect(await blocked).toBe('interrupted');
      await expect.poll(() => ws.clients.size).toBe(0);
      await save();
      const users = app.get(UsersService);
      const user = await users.create(
        admin.token,
        { username: 'bridgeuser', display_name: 'Bridge user', password: 'fixture123', role: 'user', enabled: true },
        randomUUID(),
      );
      const userLogin = await identity.login({ username: 'bridgeuser', password: 'fixture123' }, randomUUID());
      const own = await send(),
        other = await send(undefined, firstInput, userLogin);
      expect((await send(own.id, undefined, userLogin)).status).toBe(400);
      await users.update(admin.token, user.id, { enabled: false }, randomUUID());
      await expect.poll(() => ws.clients.size).toBe(1);
      expect((await send(own.id)).status).toBe(200);
      expect(other.id).toMatch(/^resp_ez_/);
      // 重启 Center 保留绑定但丢失 WS；同库重新登录后旧 ID 明确不可用。
      const beforeRestart = await config(),
        head = await send();
      await app.close();
      await expect.poll(() => ws.clients.size).toBe(0);
      app = await open();
      identity = app.get(IdentityService);
      models = app.get(ModelsService);
      admin = await identity.login({ username: 'admin', password: '123456' }, randomUUID());
      expect((await config()).server_history_binding).toBe(beforeRestart.server_history_binding);
      expect((await send(head.id)).error.code).toBe('previous_response_not_found');
      expect((await send(undefined, firstInput)).status).toBe(200);
      // 正式 Host 使用真实 Center 鉴权、配置、记录与同一个隔离 PostgreSQL。
      const home = await mkdtemp(join(root, 'host-'));
      await writeFile(
        join(home, 'host.toml'),
        `version='0.27.0'\nmode='enterprise'\n[enterprise]\ncenter_url='${origin}'\n[host_access]\nport=0\nremote_enabled=false\n`,
      );
      const hostPort = createServer();
      await new Promise<void>((done) => hostPort.listen(0, '127.0.0.1', done));
      const hp = hostPort.address();
      if (!hp || typeof hp === 'string') throw new Error('host port');
      await new Promise<void>((done) => hostPort.close(() => done()));
      await writeFile(
        join(home, 'host.toml'),
        (await readFile(join(home, 'host.toml'), 'utf8')).replace('port=0', `port=${hp.port}`),
      );
      host = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', home], {
        stdio: 'ignore',
      });
      let discovery!: { address: string; access_token: string };
      const headers = (token: string) => ({
        authorization: `Bearer ${token}`,
        'content-type': 'application/json',
        'x-ez-client-version': '0.29.0',
        'x-ez-min-compatible-version': '0.29.0',
      });
      await expect
        .poll(
          async () => {
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
      const login = await fetch(`${discovery.address}/auth/login`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'x-ez-client-version': '0.29.0',
          'x-ez-min-compatible-version': '0.29.0',
        },
        body: JSON.stringify({ method: 'enterprise', username: 'admin', password: '123456', native: true }),
      });
      expect(login.status).toBe(200);
      const token = (await login.json()).token;
      const command = async (type: string, payload: object) => {
        const response = await fetch(`${discovery.address}/commands`, {
          method: 'POST',
          headers: headers(token),
          body: JSON.stringify({ request_id: randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }),
        });
        const body = await response.json();
        expect(response.ok, JSON.stringify(body)).toBe(true);
        return body.result.payload.payload;
      };
      await command('refresh_model_source', {});
      const session_id = (
        await command('create_session', { title: 'lifecycle', workspace_id: null, model_selection: null })
      ).session.session_id;
      const run = async (message: string) => {
        const result = await command('submit_input', { session_id, message, variant: 'build' });
        await expect
          .poll(async () => {
            const { runs } = await command('list_runs', { session_id });
            return runs.find((candidate: { input_id: string; status: string }) => candidate.input_id === result.input_id)?.status;
          }, { timeout: 15000 })
          .toBe('completed');
      };
      await run('HOST_FIRST ' + 'history '.repeat(1000));
      await run('HOST_SECOND');
      expect(wire.at(-1)?.body.previous_response_id).toBeDefined();
      for (const socket of ws.clients) socket.terminate();
      await expect.poll(() => ws.clients.size).toBe(0);
      const beforeRestore = wire.length;
      await run('HOST_REBUILD');
      expect(wire.length).toBe(beforeRestore + 1);
      expect(wire.at(-1)?.body.previous_response_id).toBeUndefined();
      expect(JSON.stringify(wire.at(-1)?.body.input)).toContain('HOST_FIRST');
      await run('HOST_RESUMED');
      expect(wire.at(-1)?.body.previous_response_id).toBeDefined();
      const survivor = await send();
      const page = await command('list_conversation_page', {
        owner: { type: 'main_session', session_id },
        cursor: null,
        limit: 100,
      });
      const compact = await command('compact_session', {
        session_id,
        operation_id: randomUUID(),
        expected_generation: page.snapshot.value.generation,
      });
      expect(compact.outcome.type).toBe('compacted');
      await run('HOST_COMPACTED');
      expect(wire.at(-1)?.body.previous_response_id).toBeUndefined();
      expect(JSON.stringify(wire.at(-1)?.body.input)).not.toContain('HOST_FIRST');
      await run('HOST_COMPACTED_NEXT');
      expect(wire.at(-1)?.body.previous_response_id).toBeDefined();
      expect((await send(survivor.id)).status).toBe(200);

      await expect
        .poll(async () =>
          Number((await source.query("SELECT COUNT(*) AS n FROM llm_calls WHERE outcome='in_progress'"))[0].n),
        )
        .toBe(0);
      const complete = await source.query(
        "SELECT request_path, response_path, request_bytes FROM llm_calls WHERE outcome='forwarded' AND request_snapshot_state='complete' ORDER BY started_at LIMIT 1",
      );
      expect(complete).toHaveLength(1);
      expect(JSON.parse(await readFile(join(root, 'snapshots', complete[0].request_path), 'utf8')).input).toEqual(
        firstInput,
      );
      expect(await readFile(join(root, 'snapshots', complete[0].response_path), 'utf8')).toContain('resp_ez_');
    } finally {
      if (host && host.exitCode === null) {
        host.kill('SIGTERM');
        await new Promise<void>((done) => host!.once('exit', () => done()));
      }
      await app.close();
      await source.destroy();
      for (const socket of ws.clients) socket.terminate();
      await new Promise<void>((done) => ws.close(() => done()));
      upstream.closeAllConnections();
      await new Promise<void>((done) => upstream.close(() => done()));
    }
  },
  120000,
);
