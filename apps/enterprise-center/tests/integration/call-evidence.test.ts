import { createServer } from 'node:http';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { gzipSync } from 'node:zlib';
import { WebSocketServer } from 'ws';
import { runner } from 'node-pg-migrate';
import { expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { createHttpApp } from '../../dist/app.js';
import { emptyParameters } from '../../dist/models/parameters.js';
import { ModelProxy } from '../../dist/llm/proxy.js';
import { CallEntity } from '../../dist/llm/entity.js';

/** 每次显式新建的 F03 空库；绝不读取 CENTER_DATABASE_URL 或复用历史测试数据。 */
it.skipIf(!process.env.CENTER_F03_SCENARIO)(
  'F03 迁移、三模式、HTTP/WS 证据、管理鉴权和恢复清理闭环',
  async () => {
    const root = resolve(process.env.CENTER_F03_SCENARIO!);
    if (!['f03-m0-', 'f03-m2-'].some((prefix) => root.startsWith(resolve(`../../.runtime-test/${prefix}`))))
      throw new Error('invalid scenario root');
    const target = JSON.parse(await readFile(join(root, 'target.json'), 'utf8')) as {
      root: string;
      host: string;
      port: number;
      database: string;
    };
    if (
      target.root !== root ||
      target.host !== '127.0.0.1' ||
      !/^f03_evidence(?:_\d+)?$/.test(target.database) ||
      ![55473, 55475].includes(target.port)
    )
      throw new Error('invalid isolated target');
    const url = `postgres://f03_app@127.0.0.1:${target.port}/${target.database}`;
    const pool = createPool({ url }),
      source = createDataSource({ url });
    let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
    let mode = 'normal';
    const normal = { id: 'r', object: 'response', status: 'completed', output: [] };
    const failedFrame = JSON.stringify({
      type: 'error',
      status: 400,
      error: { message: 'fixture-private-failure', code: 'fixture_error' },
    });
    const encoded = gzipSync(Buffer.from(JSON.stringify(normal)));
    const upstream = createServer(async (request, response) => {
      for await (const _ of request) {
        /* drain the accepted fixture request */
      }
      if (mode === 'idle') return;
      response.writeHead(mode === 'http_error' ? 500 : 200, {
        'content-type': mode === 'sse_error' ? 'text/event-stream' : 'application/json',
        ...(mode === 'gzip' ? { 'content-encoding': 'gzip' } : {}),
      });
      if (mode === 'broken') {
        response.write('{"partial":');
        setTimeout(() => response.destroy(), 20);
        return;
      }
      if (mode === 'sse_error')
        response.end(
          `data: ${JSON.stringify({ type: 'response.failed', response: { ...normal, status: 'failed' } })}\n\n`,
        );
      else if (mode === 'invalid_bytes') response.end(Buffer.from([0xff, 0x61]));
      else if (mode === 'empty') response.end();
      else if (mode === 'gzip') response.end(encoded);
      else if (mode === 'unknown') response.end('{"custom":true}');
      else if (mode === 'http_error') response.end('{"error":{"message":"fixture-private-failure"}}');
      else if (request.url?.endsWith('/input_tokens')) response.end('{"input_tokens":42}');
      else if (request.url?.endsWith('/compact'))
        response.end(
          JSON.stringify({
            id: 'cmp',
            object: 'response.compaction',
            output: [{ type: 'compaction', encrypted_content: 'fixture' }],
          }),
        );
      else response.end(JSON.stringify(normal));
    });
    const ws = new WebSocketServer({ server: upstream });
    ws.on('connection', (socket) =>
      socket.on('message', () => socket.send(mode === 'ws_bad' ? '{broken' : failedFrame)),
    );
    try {
      const actual = (
        await pool.query('SELECT current_database() AS db,current_user AS role,inet_server_port() AS port')
      ).rows[0];
      expect(actual).toEqual({ db: target.database, role: 'f03_app', port: target.port });
      expect(
        (await pool.query("SELECT COUNT(*)::int AS n FROM information_schema.tables WHERE table_schema='public'"))
          .rows[0].n,
      ).toBe(0);
      // 按真实旧账本升级；两种旧开关分别用新空库验证，不重写历史 SQL。
      const oldEnabled = process.env.CENTER_F03_OLD_MODE !== 'off';
      await runner({
        databaseUrl: url,
        dir: resolve('dist/database/migrations'),
        direction: 'up',
        count: 10,
        migrationsTable: 'pgmigrations',
        singleTransaction: true,
        logger: { info() {}, warn() {}, error() {} },
      });
      await pool.query(
        'UPDATE llm_recording_settings SET enabled=$1,content_retention_days=3,index_retention_days=30',
        [oldEnabled],
      );
      const historical = randomUUID();
      await pool.query(
        "INSERT INTO llm_calls(id,kind,protocol,outcome,request_snapshot_state,response_snapshot_state,usage_state) VALUES($1,'proxy','open_ai_responses','forwarded','disabled','disabled','unavailable')",
        [historical],
      );
      await upgradeDatabase(pool);
      expect(
        (await pool.query('SELECT mode,content_retention_days,index_retention_days FROM llm_recording_settings'))
          .rows[0],
      ).toEqual({ mode: oldEnabled ? 'all' : 'off', content_retention_days: 3, index_retention_days: 30 });
      expect(
        (await pool.query('SELECT response_observation FROM llm_calls WHERE id=$1', [historical])).rows[0]
          .response_observation,
      ).toBeNull();
      await source.initialize();
      await new Promise<void>((done) => upstream.listen(0, '127.0.0.1', done));
      const address = upstream.address();
      if (!address || typeof address === 'string') throw new Error('no port');
      const snapshotRoot = join(root, target.database);
      await mkdir(snapshotRoot);
      app = await createHttpApp({ source, origin: 'http://127.0.0.1', snapshotRoot, llmIdleTimeout: 150 });
      await app.listen(0, '127.0.0.1');
      const origin = await app.getUrl();
      const login = async (username: string, password: string) => {
        const response = await fetch(`${origin}/api/auth/login`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ username, password }),
        });
        expect(response.status).toBe(200);
        return response.json();
      };
      const admin = await login('admin', '123456');
      const api = async (path: string, method = 'GET', body?: object, status = 200, token = admin.token) => {
        const response = await fetch(`${origin}/api/${path}`, {
          method,
          headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
          ...(body ? { body: JSON.stringify(body) } : {}),
        });
        const text = await response.text();
        expect(response.status, `${path}: ${text}`).toBe(status);
        return text ? JSON.parse(text) : undefined;
      };
      await api('model-settings', 'PUT', { public_origin: origin });
      const connection = {
        display_name: 'F03 fixture',
        provider_type: 'openai',
        endpoint: `http://127.0.0.1:${address.port}/v1`,
        protocol_preference: 'responses',
        models_path: '',
        discovery_format: 'openai',
        server_history_enabled: false,
        server_history_support_override: 'supported',
        responses_transport: 'http',
      };
      const provider = await api('model-providers', 'POST', { connection, credential: { mode: 'clear' } }, 201);
      const selected = { provider_instance_id: provider.provider_instance_id, model_id: 'fixture' };
      const parameters = {
        ...emptyParameters(),
        context_window_tokens: { state: 'known', value: 128000 },
        max_output_tokens: { state: 'known', value: 1024 },
        streaming: 'supported',
        tool_calls: 'supported',
        server_compaction_enabled: true,
        server_compaction_support: 'supported',
        input_token_count_support: 'supported',
      };
      await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters });
      await api('model-settings', 'PUT', { default_model: selected });
      await api(
        'users',
        'POST',
        { username: 'reader', display_name: 'reader', role: 'user', enabled: true, password: 'fixture-password1' },
        201,
      );
      const reader = await login('reader', 'fixture-password1');
      const policy = (mode: string) =>
        api('llm-recording-settings', 'PUT', { mode, content_retention_days: 3, index_retention_days: 30 });
      const proxy = app.get(ModelProxy);
      const call = async (scenario: string, operation = 'responses') => {
        mode = scenario;
        const configuration = await api('runtime/model-configuration');
        const response = await fetch(`${origin}/api/llm/providers/${provider.provider_instance_id}/v1/${operation}`, {
          method: 'POST',
          headers: {
            authorization: `Bearer ${admin.llm_key}`,
            'content-type': 'application/json',
            'x-ez-center-history-binding': configuration.server_history_binding,
          },
          body: JSON.stringify({
            model: 'fixture',
            input: [{ role: 'user', content: 'hello' }],
            ...(operation === 'responses' ? { store: false, stream: true } : {}),
          }),
        });
        try {
          await response.arrayBuffer();
        } catch {
          /* expected broken upstream */
        }
        // 响应交付先于快照结算；只等待真实 active owner，不假定 HTTP body 完成即文件落盘。
        for (let i = 0; i < 100 && proxy.records.active.size; i++) await new Promise((done) => setTimeout(done, 10));
        const page = await api('llm-calls?limit=1');
        return page.items[0];
      };
      await policy('off');
      expect((await call('http_error')).request).toMatchObject({ state: 'disabled', reason: 'policy_off' });
      await policy('anomalies');
      expect((await call('normal')).response).toMatchObject({ state: 'disabled', reason: 'normal_not_retained' });
      const failure = await call('sse_error');
      expect(failure).toMatchObject({
        outcome: 'forwarded',
        response_observation: 'anomalous',
        response: { state: 'complete' },
      });
      expect((await api(`llm-calls/${failure.id}/snapshot?side=response`)).text).toContain('response.failed');
      expect((await call('unknown')).response_observation).toBe('unconfirmed');
      const bytes = await call('invalid_bytes');
      expect(await api(`llm-calls/${bytes.id}/snapshot?side=response`)).toMatchObject({
        state: 'complete',
        text: null,
        body_base64: '/2E=',
      });
      const gzip = await call('gzip');
      expect((await api(`llm-calls/${gzip.id}/snapshot?side=response`)).body_base64).toBe(encoded.toString('base64'));
      expect((await call('empty')).response).toMatchObject({ state: 'complete', bytes: 0 });
      expect((await call('idle')).response).toMatchObject({ state: 'not_received', reason: 'upstream_idle' });
      expect((await call('broken')).response.state).toBe('partial');
      await policy('all');
      expect((await call('normal')).response).toMatchObject({ state: 'complete' });
      expect(await call('normal', 'responses/input_tokens')).toMatchObject({
        response_observation: 'normal',
        usage_state: 'unavailable',
      });
      expect(await call('normal', 'responses/compact')).toMatchObject({
        response_source: 'upstream_http',
        response_observation: 'normal',
        response: { state: 'complete' },
      });
      mode = 'normal';
      await api('model-configuration/test', 'POST', selected);
      await api(`model-providers/${provider.provider_instance_id}`, 'PUT', {
        connection: { ...connection, responses_transport: 'websocket' },
        credential: { mode: 'unchanged' },
      });
      const wsFailure = await call('ws_error');
      expect(wsFailure).toMatchObject({
        reason: 'response_error',
        response_observation_reason: 'response_error',
        response_source: 'websocket_bridge',
        response: { state: 'not_received' },
        upstream_failure: { state: 'complete' },
      });
      expect((await api(`llm-calls/${wsFailure.id}/snapshot?side=upstream_failure`)).text).toBe(failedFrame);
      const bad = await call('ws_bad');
      expect((await api(`llm-calls/${bad.id}/snapshot?side=upstream_failure`)).text).toBe('{broken');
      expect(await call('ws_error', 'responses/compact')).toMatchObject({
        response_source: 'websocket_compact',
        response: { state: 'not_received' },
        upstream_failure: { state: 'complete' },
      });
      await api(`llm-calls/${bad.id}/snapshot?side=response`, 'GET', undefined, 403, reader.token);
      await api(
        'llm-recording-settings',
        'PUT',
        { enabled: true, content_retention_days: 7, index_retention_days: 90 },
        400,
      );
      expect(
        (await api('llm-calls?response_observation=anomalous')).items.every(
          (item: CallEntity) => item.response_observation === 'anomalous',
        ),
      ).toBe(true);
      // 准确保留非文本与失败帧，重启恢复不依赖 UTF-8；到期先文件后索引。
      await app.close();
      app = undefined;
      if (source.isInitialized) await source.destroy();
      const recoverySource = createDataSource({ url });
      await recoverySource.initialize();
      app = await createHttpApp({ source: recoverySource, origin, snapshotRoot });
      const records = app.get(ModelProxy).records;
      expect(
        (await recoverySource.getRepository(CallEntity).findOneByOrFail({ id: bytes.id })).response_snapshot_state,
      ).toBe('complete');
      await pool.query("UPDATE llm_calls SET ended_at=now()-interval '5 days' WHERE id=$1", [wsFailure.id]);
      await records.cleanup();
      const expired = await recoverySource.getRepository(CallEntity).findOneByOrFail({ id: wsFailure.id });
      expect(expired.upstream_failure_snapshot?.state).toBe('expired');
      expect(await records.files.read(expired.id, expired.started_at, 'upstream_failure')).toBeNull();
      const audit = (
        await pool.query(
          "SELECT details::text FROM management_audit WHERE action IN ('llm_snapshot_viewed','llm_recording_settings_changed')",
        )
      ).rows;
      expect(JSON.stringify(audit)).not.toContain('fixture-private-failure');
      const counts: Record<string, string> = {};
      for (const { tablename } of (
        await pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")
      ).rows) {
        if (!/^[a-z_]+$/.test(tablename)) throw new Error('unexpected table');
        counts[tablename] = (await pool.query(`SELECT COUNT(*)::text AS n FROM public.${tablename}`)).rows[0].n;
      }
      await writeFile(join(root, `${target.database}-counts.json`), JSON.stringify(counts));
      expect(counts.pgmigrations).toBe(String(upgradeFiles().names.length));
      expect(counts.providers).toBe('1');
      expect(counts.users).toBe('2');
      await app.close();
      app = undefined;
      await recoverySource.destroy();
      await writeFile(join(root, 'browser-case.json'), JSON.stringify({ database: target.database, snapshotRoot }));
    } finally {
      await app?.close();
      for (const socket of ws.clients) socket.terminate();
      await new Promise<void>((done) => ws.close(() => done()));
      upstream.closeAllConnections();
      if (upstream.listening) await new Promise<void>((done) => upstream.close(() => done()));
      if (source.isInitialized) await source.destroy();
      await pool.end();
    }
  },
  30000,
);
