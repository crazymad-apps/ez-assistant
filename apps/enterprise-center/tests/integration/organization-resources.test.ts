import { defaultHistoryModel, singleModelPolicy } from '../support/model-policy.js';
import { once } from 'node:events';
import { createHash, randomUUID } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { mkdir } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { runner } from 'node-pg-migrate';
import { expect, it } from 'vitest';
import { WebSocketServer } from 'ws';
import { ZipFile } from 'yazl';
import { createHttpApp } from '../../dist/app.js';
import type { RuntimeModelConfiguration, ModelSettings } from '../../dist/contracts/models.js';
import type { LoginResponse } from '../../dist/contracts/identity.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { emptyParameters } from '../../dist/models/parameters.js';
import { ModelProxy } from '../../dist/llm/proxy.js';

const url = process.env.CENTER_F01_M1_DATABASE_URL;
const home = process.env.CENTER_F01_M1_TEST_HOME;
if (!url || !home || !realpathSync(home).startsWith(`${realpathSync('../../.runtime-test')}/v033-f01-m1.`))
  throw new Error('需要新建并显式指定 F01 M1 隔离库。');
const target = new URL(url);
if (target.hostname !== '127.0.0.1' || !target.port || target.pathname !== '/ez_center_v033_f01_m1' ||
    target.username !== 'f01_m1' || target.search || target.hash)
  throw new Error('隔离库目标不匹配。');

async function packageBytes(content: string, name = 'document-tools') {
  const archive = new ZipFile();
  archive.addBuffer(Buffer.from(JSON.stringify({
    format_version: 1, name, display_name: '文档工具', description: '文档工具',
    version: '1.0.0', platforms: [{ os: 'macos', arch: 'arm64' }], min_host_version: '0.28.0',
    skills: ['document-review'], mcp: [], configuration: [], external_software: [],
  })), 'plugin.json');
  archive.addBuffer(Buffer.from('---\nname: document-review\ndescription: Review documents\n---\n' + content),
    'skills/document-review/SKILL.md');
  archive.end();
  const chunks: Buffer[] = [];
  for await (const chunk of archive.outputStream) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks);
}

it('旧版资源迁入默认组织；两组织通过真实 HTTP 读写及撤权', async () => {
  const pool = createPool({ url });
  const source = createDataSource({ url });
  const upstream = createServer((req, res) => {
    let body = '';
    req.setEncoding('utf8').on('data', (chunk) => { body += chunk; });
    req.once('end', () => {
      const { model } = JSON.parse(body);
      res.writeHead(200, { 'content-type': 'application/json' });
      if (req.url?.endsWith('/input_tokens')) {
        res.end(JSON.stringify({ input_tokens: 42 }));
        return;
      }
      if (req.url?.endsWith('/compact')) {
        res.end(JSON.stringify({ id: 'cmp_fixture', object: 'response.compaction', output: [] }));
        return;
      }
      res.end(JSON.stringify({ id: 'upstream-response', model, object: 'response', status: 'completed', output: [],
        usage: { input_tokens: 11, output_tokens: 7, total_tokens: 18 } }));
    });
  });
  const ws = new WebSocketServer({ server: upstream });
  ws.on('connection', socket => socket.on('message', (message) => {
    const { model } = JSON.parse(message.toString());
    socket.send(JSON.stringify({ type: 'response.created', response: { id: 'resp_ws_fixture', status: 'in_progress' } }));
    socket.send(JSON.stringify({ type: 'response.completed', response: { id: 'resp_ws_fixture', model, status: 'completed', output: [] } }));
  }));
  const device = randomUUID();
  const oldProvider = randomUUID();
  const oldCall = randomUUID();
  const oldFeedback = randomUUID();
  const submissionId = randomUUID();
  let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
  try {
    const verified = await pool.query(`SELECT host(inet_server_addr()) AS host, inet_server_port() AS port,
      current_database() AS db, current_user AS role, current_setting('data_directory') AS dir,
      (SELECT count(*) FROM pg_tables WHERE schemaname = 'public') AS tables`);
    expect(verified.rows[0]).toEqual({ host: target.hostname, port: Number(target.port), db: target.pathname.slice(1),
      role: target.username, dir: join(home, 'pgdata'), tables: '0' });
    await new Promise<void>(resolve => upstream.listen(0, '127.0.0.1', resolve));
    const address = upstream.address();
    if (!address || typeof address === 'string') throw new Error('missing upstream port');
    const connection = {
      display_name: '隔离上游', provider_type: 'openai', endpoint: `http://127.0.0.1:${address.port}/v1`,
      protocol_preference: 'responses', models_path: '', discovery_format: 'openai',
      server_history_enabled: true, server_history_support_override: 'supported', responses_transport: 'http',
    };
    const parameters = emptyParameters();
    parameters.context_window_tokens = { state: 'known', value: 16384 };
    parameters.max_output_tokens = { state: 'known', value: 1024 };
    const originSetting = 'http://127.0.0.1:7329';
    const secret = 'd'.repeat(64);
    const old = await pool.connect();
    try {
      const files = upgradeFiles();
      expect(files.names.slice(12)).toEqual(['013-organizations', '014-organization-resources', '015-organization-model-policy']);
      expect(await runner({ dbClient: old, dir: files.directory, direction: 'up', count: 12, singleTransaction: true,
        migrationsTable: 'pgmigrations', schema: 'public', migrationsSchema: 'public',
        logger: { info() {}, warn() {}, error() {} } })).toHaveLength(12);
      await old.query('BEGIN');
      try {
        await old.query(`INSERT INTO public.users(username, display_name, role, password_hash)
          SELECT 'm1-reader', '测试成员', 'user', password_hash FROM public.users WHERE username='admin'`);
        await old.query(`INSERT INTO public.providers(provider_instance_id, connection, server_history_key)
          VALUES ($1, $2, 'legacy-history-key')`, [oldProvider, connection]);
        await old.query(`INSERT INTO public.model_fixed_configs(provider_instance_id, model_id, origin, parameters)
          VALUES ($1, 'fixture-model', 'manual', $2)`, [oldProvider, parameters]);
        await old.query(`UPDATE public.model_settings SET provider_instance_id=$1, model_id='fixture-model', public_origin=$2`,
          [oldProvider, originSetting]);
        await old.query("UPDATE public.llm_recording_settings SET mode='all', content_retention_days=2");
        await old.query(`INSERT INTO public.llm_calls(id, provider_instance_id, model_id, kind, protocol, outcome,
          ended_at, request_snapshot_state, response_snapshot_state, usage_state)
          VALUES ($1, $2, 'fixture-model', 'proxy', 'open_ai_responses', 'forwarded', now(), 'disabled', 'disabled', 'unavailable')`,
        [oldCall, oldProvider]);
        await old.query(`INSERT INTO public.plugins(name, display_name, description)
          VALUES ('legacy-plugin', '旧插件', '旧插件')`);
        await old.query(`INSERT INTO public.plugin_packages(plugin_name, version, manifest, archive_format, sha256, size_bytes)
          VALUES ('legacy-plugin', '1.0.0', $1, 'zip', $2, 1)`, [{ skills: [], mcp: [], platforms: [] }, 'a'.repeat(64)]);
        await old.query(`INSERT INTO public.custom_agents(name, description, prompt, skills, mcp)
          VALUES ('shared-agent', '旧组织', '旧提示词', '[]', '[]')`);
        await old.query(`INSERT INTO public.feedback(id, owner_user_id, owner_username, owner_display_name, submission_id,
          source, target_kind, target_reference, target_name, title, description, environment)
          SELECT $1, id, username, display_name, $2, 'manual', 'platform', 'ez-assistant', 'EZ Assistant', '旧反馈', '旧反馈', '{}'
          FROM public.users WHERE username='admin'`, [oldFeedback, submissionId]);
        await old.query(`INSERT INTO public.host_devices(id, installation_id, credential_hash, name, platform, host_version, applicant_user_id, status)
          SELECT $1, $2, $3, '旧设备', 'macos', '0.32.2', id, 'registered' FROM public.users WHERE username='admin'`,
        [device, randomUUID(), createHash('sha256').update(secret).digest('hex')]);
        await old.query(`INSERT INTO public.host_device_users(device_id, user_id, desktop_view)
          SELECT $1, id, true FROM public.users WHERE username='m1-reader'`, [device]);
        await old.query(`INSERT INTO public.management_audit(action, success, request_id) VALUES
          ('agent_created', true, gen_random_uuid()), ('unknown_legacy_action', true, gen_random_uuid())`);
        await old.query('COMMIT');
      } catch (error) {
        await old.query('ROLLBACK');
        throw error;
      }
    } finally {
      old.release(true);
    }
    expect((await upgradeDatabase(pool)).applied).toEqual(['013-organizations', '014-organization-resources', '015-organization-model-policy']);
    const [center] = (await pool.query('SELECT center_id, legacy_organization_id, public_origin FROM public.center_identity')).rows;
    const legacy = center.legacy_organization_id as string;
    expect(center.public_origin).toBe(originSetting);
    for (const table of ['providers', 'model_settings', 'organization_models', 'llm_calls', 'llm_recording_settings', 'plugins', 'plugin_packages',
      'custom_agents', 'feedback', 'host_devices', 'host_device_users']) {
      expect((await pool.query(`SELECT organization_id, count(*) FROM public.${table} GROUP BY organization_id`)).rows,
        table).toEqual([{ organization_id: legacy, count: '1' }]);
    }
    expect((await pool.query('SELECT count(*) FROM public.model_fixed_configs WHERE provider_instance_id=$1', [oldProvider])).rows[0].count)
      .toBe('1');
    expect((await pool.query(`SELECT action, scope, organization_id FROM public.management_audit
      WHERE action IN ('agent_created','unknown_legacy_action') ORDER BY action`)).rows).toEqual([
      { action: 'agent_created', scope: 'organization', organization_id: legacy },
      { action: 'unknown_legacy_action', scope: 'system', organization_id: null },
    ]);

    const snapshotRoot = join(home, 'snapshots'), pluginRoot = join(home, 'plugins');
    await mkdir(snapshotRoot, { mode: 0o700 });
    await mkdir(pluginRoot, { mode: 0o700 });
    await source.initialize();
    app = await createHttpApp({ source, origin: originSetting, snapshotRoot, pluginRoot });
    await app.listen(0, '127.0.0.1');
    const origin = await app.getUrl();
    async function request(
      method: string, path: string, token: string | undefined, status: number,
      payload?: object | Buffer, extra?: Record<string, string>,
    ) {
      const raw = Buffer.isBuffer(payload);
      const response = await fetch(`${origin}/api/${path}`, { method,
        headers: { ...(token ? { authorization: `Bearer ${token}` } : {}),
          ...(payload ? { 'content-type': raw ? 'application/zip' : 'application/json' } : {}), ...extra },
        body: payload ? raw ? new Uint8Array(payload) : JSON.stringify(payload) : undefined,
      });
      if (response.status !== status) throw new Error(`${method} ${path}: expected ${status}, got ${response.status}: ${await response.text()}`);
      return response;
    }
    async function login(username = 'admin') {
      return (await request('POST', 'auth/login', undefined, 200, { username, password: '123456' })).json() as Promise<LoginResponse>;
    }
    async function select(user: LoginResponse, organization_id?: string, entry = 'admin') {
      return (await request('POST', 'auth/scope', user.token, 200,
        { kind: organization_id ? 'organization' : 'system', ...(organization_id ? { organization_id } : {}), entry })).json() as Promise<LoginResponse>;
    }
    const system = await select(await login());
    const reader = await login('m1-reader');
    const beta = await (await request('POST', 'organizations', system.token, 201,
      { name: '第二组织', code: 'beta', administrator_user_id: reader.user.id })).json() as { id: string };
    const alpha = await select(await login(), legacy);
    const betaManager = await select(await login(), beta.id);
    const betaReader = await select(reader, beta.id, 'client');
    const alphaMember = await select(await login(), legacy, 'client');

    await request('GET', 'model-providers', system.token, 403);
    expect(await (await request('GET', 'model-settings', system.token, 200)).json()).toMatchObject({
      public_origin: originSetting, policy: null,
    });
    expect(await (await request('GET', 'model-settings', alpha.token, 200)).json()).toMatchObject({
      policy: { default_model: { provider_instance_id: oldProvider, model_id: 'fixture-model' }, allow_member_selection: false },
      availability: [{ state: 'ready', model_id: 'fixture-model' }],
    });
    await request('GET', `model-providers/${oldProvider}/fixed-configs`, betaManager.token, 404);
    await request('PUT', 'model-settings', betaManager.token, 403, { public_origin: 'https://beta.invalid' });
    await request('GET', 'model-templates', betaManager.token, 403);
    await request('GET', 'runtime/model-configuration', betaManager.token, 403);
    const provider = await (await request('POST', 'model-providers', betaManager.token, 201,
      { connection, credential: { mode: 'clear' } })).json() as { provider_instance_id: string };
    const selection = { provider_instance_id: provider.provider_instance_id, model_id: 'fixture-model' };
    await request('PUT', 'model-configuration', betaManager.token, 200, { ...selection, origin: 'manual', parameters });
    await request('PUT', 'model-settings', betaManager.token, 200, singleModelPolicy(await (await request('GET', 'model-settings', betaManager.token, 200)).json(), selection));
    await request('PUT', 'model-settings', alpha.token, 404, singleModelPolicy(await (await request('GET', 'model-settings', alpha.token, 200)).json(), selection));
    const alphaModel = defaultHistoryModel(await (await request('GET', 'runtime/model-configuration', alphaMember.token, 200)).json());
    const betaModel = defaultHistoryModel(await (await request('GET', 'runtime/model-configuration', betaReader.token, 200)).json());
    expect(alphaModel.endpoint).toContain(oldProvider);
    expect(betaModel.endpoint).toContain(provider.provider_instance_id);
    expect(alphaModel.server_history_binding).not.toBe(betaModel.server_history_binding);

    expect(await (await request('GET', 'llm-recording-settings', betaManager.token, 200)).json())
      .toMatchObject({ mode: 'off', content_retention_days: 7 });
    const generate = { model: 'fixture-model', store: false, input: 'fixture' };
    await request('POST', `llm/providers/${oldProvider}/v1/responses`, betaReader.llm_key, 409, generate);
    const result = await request('POST', `llm/providers/${oldProvider}/v1/responses`, alphaMember.llm_key, 200, generate);
    const callId = result.headers.get('x-ez-call-id')!;
    expect(await result.json()).toMatchObject({ id: 'upstream-response' });
    await request('GET', `llm-calls/${callId}/snapshot?side=request`, betaManager.token, 404);
    await expect.poll(async () => (await (await request('GET', `llm-calls/${callId}`, alpha.token, 200)).json()).request.state)
      .toBe('complete');
    const snapshot = await (await request('GET', `llm-calls/${callId}/snapshot?side=request`, alpha.token, 200)).json();
    expect(snapshot.text).toContain('fixture');
    expect(await (await request('GET', 'llm-calls', alpha.token, 200)).json()).toMatchObject({ total: 2 });
    expect(await (await request('GET', 'llm-calls', betaManager.token, 200)).json()).toMatchObject({ total: 1 });
    expect(await (await request('GET', 'audit?action=llm_snapshot_viewed', alpha.token, 200)).json())
      .toMatchObject({ total: 1, items: [{ details: { call_id: callId } }] });
    expect(await (await request('GET', 'audit?action=llm_snapshot_viewed', system.token, 200)).json()).toMatchObject({ total: 0 });
    const usage = await (await request('GET', 'llm-usage', alpha.token, 200)).json();
    expect(usage.summary).toMatchObject({ calls: '2', total_tokens: '18' });

    const bytes = await packageBytes('same bytes for both');
    const upload = (token: string, data: Buffer) => request('POST', 'plugin-packages', token, 201, data);
    const sha = (await (await upload(alpha.token, bytes)).json() as { sha256: string }).sha256;
    await upload(alpha.token, await packageBytes('only alpha', 'alpha-only'));
    expect((await (await upload(betaManager.token, bytes)).json() as { sha256: string }).sha256).toBe(sha);
    for (const [token, name] of [[alpha.token, 'document-tools'], [betaManager.token, 'document-tools'], [alpha.token, 'alpha-only']] as const) {
      const detail = await request('GET', `plugins/${name}`, token, 200);
      await request('PUT', `plugins/${name}/publication`, token, 200, { listed: true },
        { 'if-match': detail.headers.get('etag')! });
    }
    const download = (token: string, plugin: string, status: number) => request('GET',
      `runtime/plugins/${plugin}/versions/1.0.0/package`, token, status, undefined, { 'if-match': `"${sha}"` });
    expect(Buffer.from(await (await download(betaReader.token, 'document-tools', 200)).arrayBuffer())).toEqual(bytes);
    await download(betaReader.token, 'alpha-only', 404);
    const agent = { name: 'shared-agent', description: '第二组织', prompt: '独立提示词', skills: [], mcp: [] };
    await request('POST', 'agents', betaManager.token, 409,
      { ...agent, skills: [{ name: 'document-review', plugin: 'alpha-only' }] });
    await request('POST', 'agents', betaManager.token, 201, agent);
    expect(await (await request('GET', 'agents/shared-agent', alpha.token, 200)).json()).toMatchObject({ prompt: '旧提示词' });

    await request('GET', `admin/feedback/${oldFeedback}`, betaManager.token, 404);
    expect(await (await request('GET', 'admin/feedback', alpha.token, 200)).json()).toMatchObject({ total: 1 });
    await request('GET', `admin/host-devices/${device}`, betaManager.token, 404);
    await request('GET', `host-devices/${device}/registration`, secret, 200);
    const alphaReader = await select(await login('m1-reader'), legacy, 'client');
    await request('PUT', 'model-settings', alphaReader.token, 403, singleModelPolicy(
      await (await request('GET', 'model-settings', alpha.token, 200)).json(), null));
    const feedback = { target: { kind: 'platform', name: 'EZ Assistant' },
      title: '共享提交标识', description: '各组织单独提交', submission_id: submissionId,
      source: 'manual', environment: { client_kind: 'desktop' } };
    const firstFeedback = await (await request('POST', 'feedback', alphaReader.token, 200, feedback)).json();
    const secondFeedback = await (await request('POST', 'feedback', betaReader.token, 200, feedback)).json();
    expect(firstFeedback.id).not.toBe(secondFeedback.id);
    await request('GET', `feedback/${firstFeedback.id}`, betaReader.token, 404);
    const install = { center_id: center.center_id, id: randomUUID(), installation_id: randomUUID(),
      secret: 'b'.repeat(64), name: '第二组织设备', platform: 'macos', host_version: '0.33.0' };
    await request('POST', 'host-devices', betaManager.token, 403, install);
    await request('POST', 'host-devices', betaReader.token, 200, install);
    expect(await (await request('GET', 'admin/host-devices?status=unregistered', betaManager.token, 200)).json())
      .toMatchObject({ total: 1, items: [{ id: install.id }] });
    const expected = { name: install.name, status: 'pending', enabled: true, users: [] };
    const grant = { desktop_view: true, desktop_control: false };
    const manage = (user_id: number) => ({ expected, desired: { ...expected, status: 'registered', users: [{ ...grant, user_id }] } });
    await request('PATCH', `admin/host-devices/${install.id}`, betaManager.token, 409, manage(system.user.id));
    await request('PATCH', `admin/host-devices/${install.id}`, betaReader.token, 200, manage(reader.user.id));
    expect(await (await request('GET', 'organization-members?active_only=true', betaManager.token, 200)).json())
      .toMatchObject({ total: 1, items: [{ user_id: reader.user.id }] });
    await request('PUT', `organization-members/${reader.user.id}`, alpha.token, 204, { role: 'member', enabled: false });
    expect((await pool.query('SELECT device_id FROM public.host_device_users WHERE user_id=$1', [reader.user.id])).rows)
      .toEqual([{ device_id: install.id }]);
    expect(await (await request('GET', 'audit?action=unknown_legacy_action', system.token, 200)).json()).toMatchObject({ total: 1 });
    expect(await (await request('GET', 'audit?action=unknown_legacy_action', alpha.token, 200)).json()).toMatchObject({ total: 0 });

    await pool.query(`UPDATE public.llm_calls SET started_at=now()-interval '10 days', ended_at=now()-interval '10 days'
      WHERE id = $1 OR organization_id = $2`, [oldCall, beta.id]);
    await request('PUT', 'llm-recording-settings', alpha.token, 200,
      { mode: 'all', content_retention_days: 1, index_retention_days: 5 });
    await app.get(ModelProxy).records.cleanup();
    expect((await pool.query('SELECT organization_id, count(*) FROM public.llm_calls GROUP BY organization_id ORDER BY organization_id')).rows)
      .toEqual([{ organization_id: legacy, count: '1' }, { organization_id: beta.id, count: '1' }]
        .sort((a, b) => a.organization_id.localeCompare(b.organization_id)));
    const alphaEndpoint = `llm/providers/${oldProvider}/v1/responses`;
    const alphaHeaders = { 'x-ez-center-history-binding': alphaModel.server_history_binding };
    const first = await request('POST', alphaEndpoint, alphaMember.llm_key, 200, { ...generate, store: true }, alphaHeaders);
    const previous = (await first.json()).id;
    const continuation = { ...generate, store: true, previous_response_id: previous };
    const denied = await request('POST', `llm/providers/${provider.provider_instance_id}/v1/responses`,
      betaReader.llm_key, 400, continuation, { 'x-ez-center-history-binding': betaModel.server_history_binding });
    expect(await denied.json()).toMatchObject({ error: { code: 'previous_response_not_found' } });
    await request('POST', alphaEndpoint, alphaMember.llm_key, 200, continuation, alphaHeaders);
    await request('PUT', `model-providers/${provider.provider_instance_id}`, betaManager.token, 200,
      { connection: { ...connection, responses_transport: 'websocket' }, credential: { mode: 'unchanged' } });
    const managed = defaultHistoryModel(await (await request('GET', 'runtime/model-configuration', betaReader.token, 200)).json());
    const opened = once(ws, 'connection');
    const wire = { model: 'fixture-model', stream: true, store: true, input: [] };
    const endpoint = `llm/providers/${provider.provider_instance_id}/v1/responses`;
    const bridgeHeaders = { 'x-ez-center-history-binding': managed.server_history_binding };
    const bridged = await request('POST', endpoint, betaReader.llm_key, 200, wire, bridgeHeaders);
    const events = (await bridged.text()).split('\n').filter(line => line.startsWith('data: '));
    const completed = events.map(line => JSON.parse(line.slice(6))).find(event => event.type === 'response.completed');
    expect(completed.response.id).toMatch(/^resp_/);
    const [socket] = await opened;
    const another = await select(await login('m1-reader'), beta.id, 'client');
    await request('POST', endpoint, another.llm_key, 400,
      { ...wire, previous_response_id: completed.response.id }, bridgeHeaders);
    const closed = once(socket, 'close');
    await request('POST', 'auth/logout', betaReader.token, 204, {});
    await closed;
    await request('POST', 'auth/logout', alphaMember.token, 204, {});
    await request('POST', `llm/providers/${oldProvider}/v1/responses`, alphaMember.llm_key, 401, generate);

    // M0 在相同的合成旧库上验证多模型策略及真实调用，避免另造一套迁移夹具。
    const member = await select(await login('m1-reader'), beta.id, 'client');
    const settings = async () => (await request('GET', 'model-settings', betaManager.token, 200)).json() as Promise<ModelSettings>;
    const runtime = async () => (await request('GET', 'runtime/model-configuration', member.token, 200)).json() as Promise<RuntimeModelConfiguration>;
    const savePolicy = async (policy: NonNullable<ModelSettings['policy']>, status = 200) =>
      (await request('PUT', 'model-settings', betaManager.token, status, { policy })).json() as Promise<ModelSettings>;
    const alternate = { ...selection, model_id: 'alternate' };
    const secondProvider = await (await request('POST', 'model-providers', betaManager.token, 201,
      { connection, credential: { mode: 'replace', value: 'm0-upstream-key' } })).json() as { provider_instance_id: string };
    const sameName = { ...selection, provider_instance_id: secondProvider.provider_instance_id };
    const compactParameters = { ...parameters, server_compaction_enabled: true,
      server_compaction_support: 'supported', input_token_count_support: 'supported' };
    for (const ref of [alternate, sameName])
      await request('PUT', 'model-configuration', betaManager.token, 200,
        { ...ref, origin: 'manual', parameters: compactParameters });
    const previousPolicy = (await settings()).policy!;
    const expanded = { ...previousPolicy, allowed_models: [selection, alternate, sameName], allow_member_selection: true };
    await savePolicy({ ...expanded, allowed_models: [selection, selection] }, 400);
    await savePolicy({ ...expanded, default_model: null }, 400);
    await savePolicy(expanded);
    await savePolicy(expanded, 409);
    await request('PUT', 'model-settings', system.token, 403, { policy: expanded });
    const effective = await runtime();
    expect(effective.organization_id).toBe(beta.id);
    expect(effective.providers).toHaveLength(2);
    expect(JSON.stringify(effective)).not.toContain('m0-upstream-key');
    expect(JSON.stringify(effective)).not.toContain(connection.endpoint);
    const firstGroup = effective.providers.find((row) => row.provider_instance_id === selection.provider_instance_id)!;
    const second = effective.providers.find((row) => row.provider_instance_id === sameName.provider_instance_id)!;
    expect(firstGroup.endpoint).not.toBe(second.endpoint);
    expect(firstGroup.models.map((row) => row.model_id)).toEqual(['alternate', 'fixture-model']);
    expect(second.models.map((row) => row.model_id)).toEqual(['fixture-model']);
    const firstBinding = firstGroup.models.find((row) => row.model_id === selection.model_id)?.server_history_binding;
    const alternateBinding = firstGroup.models.find((row) => row.model_id === alternate.model_id)?.server_history_binding;
    expect(firstBinding).toBe(managed.server_history_binding);
    expect(alternateBinding).toBeTruthy();
    expect(firstBinding).not.toBe(alternateBinding);
    expect(firstBinding).not.toBe(second.models[0]?.server_history_binding);
    const input = [{ role: 'user', content: 'fixture' }];
    const alternateRequest = { ...generate, input, model: alternate.model_id };
    const alternateResult = await request('POST', endpoint, member.llm_key, 200, { ...alternateRequest, stream: true });
    expect(await alternateResult.text()).toContain('"model":"alternate"');
    const secondEndpoint = `llm/providers/${sameName.provider_instance_id}/v1/responses`;
    const sameNameResult = await request('POST', secondEndpoint, member.llm_key, 200, generate);
    expect(await sameNameResult.json()).toMatchObject({ model: 'fixture-model' });
    const secondHeaders = { 'x-ez-center-history-binding': second.models[0]!.server_history_binding! };
    const auxiliary = { model: sameName.model_id, input };
    await request('POST', `${secondEndpoint}/compact`, member.llm_key, 200, auxiliary, secondHeaders);
    expect(await (await request('POST', `${secondEndpoint}/input_tokens`, member.llm_key, 200, auxiliary, secondHeaders)).json())
      .toEqual({ input_tokens: 42 });
    const compactBody = { model: alternate.model_id, input };
    const compactHeaders = { 'x-ez-center-history-binding': alternateBinding! };
    const enabled = (await settings()).policy!;
    await savePolicy({ ...enabled, allow_member_selection: false });
    expect((await settings()).policy?.allowed_models).toHaveLength(3);
    expect((await runtime()).providers.flatMap((row) => row.models).map((row) => row.model_id)).toEqual([selection.model_id]);
    await request('POST', endpoint, member.llm_key, 409, { ...alternateRequest, stream: true });
    await request('POST', `${endpoint}/compact`, member.llm_key, 409, compactBody, compactHeaders);
    await request('POST', `${endpoint}/input_tokens`, member.llm_key, 409, compactBody, compactHeaders);
    await request('POST', secondEndpoint, member.llm_key, 409, generate);
    await request('POST', 'model-configuration/test', betaManager.token, 200, sameName);
    const disabled = (await settings()).policy!;
    await savePolicy({ ...disabled, allow_member_selection: true });
    const reenabled = (await runtime()).providers.find((row) => row.provider_instance_id === selection.provider_instance_id)!;
    expect(reenabled.models.find((row) => row.model_id === alternate.model_id)?.server_history_binding).not.toBe(alternateBinding);
    await request('DELETE', `model-providers/${sameName.provider_instance_id}`, betaManager.token, 200);
    const unavailable = (await runtime()).providers.find((row) => row.provider_instance_id === sameName.provider_instance_id);
    expect(unavailable?.models[0]).toMatchObject({ state: 'unavailable', reason: 'provider_deleted' });
    expect((await settings()).availability).toHaveLength(3);
    expect((await pool.query('SELECT count(*) FROM public.organization_models WHERE organization_id=$1', [beta.id])).rows[0].count)
      .toBe('3');
    const finalPolicy = (await settings()).policy!;
    await savePolicy({ ...finalPolicy, default_model: null, allowed_models: [] });
    expect((await runtime()).providers).toEqual([]);
    expect((await pool.query('SELECT count(*) FROM public.organization_models WHERE organization_id=$1', [beta.id])).rows[0].count)
      .toBe('0');

  } finally {
    await app?.close();
    if (source.isInitialized) await source.destroy();
    await pool.end();
    for (const socket of ws.clients) socket.terminate();
    ws.close();
    upstream.closeAllConnections();
    await new Promise<void>(resolve => upstream.close(() => resolve()));
  }
}, 30000);
