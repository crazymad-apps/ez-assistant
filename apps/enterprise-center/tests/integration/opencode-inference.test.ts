import { randomUUID, createHash } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { afterAll, expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { emptyParameters } from '../../dist/models/parameters.js';

// 显式新建空场景；不使用用户 Runtime Home、共享库或真实模型账号。
const root = resolve(process.env.CENTER_F05_INFERENCE_SCENARIO ?? '');
if (!process.env.CENTER_F05_INFERENCE_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f05-inference-')))
  throw new Error('必须使用全新 F05 推理隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string; postgres_port: number; center_port: number; upstream_port: number; local_port: number; enterprise_port: number;
};
if (scenario.root !== root) throw new Error('场景目录不符');
const origin = `http://127.0.0.1:${scenario.center_port}`;
const url = `postgres://f05_inference@127.0.0.1:${scenario.postgres_port}/f05_inference`;
const pool = createPool({ url }), source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
type Selection = { provider_instance_id: string; model_id: string };
type Session = { session_id: string; title: string };
type Call = { body: Buffer; path: string; route: string; agent: string; authorization: string };
type ToolCall = { name: string; arguments: object };
const calls: Call[] = [];
let sequence = 0;
const image = join(root, 'fixture.png');
function routing(session: string, child?: string) {
  const length = Buffer.alloc(8); length.writeBigUInt64BE(BigInt(Buffer.byteLength(session)));
  return createHash('sha256').update('ez-assistant/conversation-routing/v1\0').update(length).update(session)
    .update(child ? `child\0${child}` : 'main\0').digest('hex');
}
function sse(responses: boolean, model: string, tools: ToolCall[]) {
  const id = `response_${++sequence}`, data = (value: object) => `data: ${JSON.stringify(value)}\n\n`;
  if (!responses) return data({ id, model, choices: [{ index: 0, delta: tools.length ? {
    tool_calls: tools.map((tool, index) => ({ index, id: `${id}_call_${index}`, type: 'function', function: { name: tool.name, arguments: JSON.stringify(tool.arguments) } })),
  } : { content: 'Fixture answer' }, finish_reason: null }] }) +
    data({ id, model, choices: [{ index: 0, delta: {}, finish_reason: tools.length ? 'tool_calls' : 'stop' }] }) + 'data: [DONE]\n\n';
  let text = data({ type: 'response.created', response: { id, model, status: 'in_progress' } });
  if (tools.length) tools.forEach((tool, index) => {
    const item = { id: `${id}_item_${index}`, type: 'function_call', call_id: `${id}_call_${index}`, name: tool.name, arguments: JSON.stringify(tool.arguments), status: 'completed' };
    text += data({ type: 'response.output_item.added', output_index: index, item: { ...item, arguments: '', status: 'in_progress' } });
    text += data({ type: 'response.function_call_arguments.delta', item_id: item.id, output_index: index, delta: item.arguments });
    text += data({ type: 'response.function_call_arguments.done', item_id: item.id, output_index: index, name: item.name, arguments: item.arguments });
    text += data({ type: 'response.output_item.done', output_index: index, item });
  });
  else {
    const item = `${id}_message`, part = { type: 'output_text', text: 'Fixture answer', annotations: [] };
    text += data({ type: 'response.output_item.added', output_index: 0, item: { id: item, type: 'message', role: 'assistant', status: 'in_progress', content: [] } });
    text += data({ type: 'response.content_part.added', item_id: item, output_index: 0, content_index: 0, part: { ...part, text: '' } });
    text += data({ type: 'response.output_text.delta', item_id: item, output_index: 0, content_index: 0, delta: part.text });
    text += data({ type: 'response.output_text.done', item_id: item, output_index: 0, content_index: 0, text: part.text });
    text += data({ type: 'response.content_part.done', item_id: item, output_index: 0, content_index: 0, part });
    text += data({ type: 'response.output_item.done', output_index: 0, item: { id: item, type: 'message', role: 'assistant', status: 'completed', content: [part] } });
  }
  return text + data({ type: 'response.completed', response: { id, model, status: 'completed', usage: { input_tokens: 20, output_tokens: 5, total_tokens: 25 } } }) + 'data: [DONE]\n\n';
}
const upstream = createServer(async (request, response) => {
  if (request.method === 'GET') {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ data: [{ id: 'main-fixture' }, { id: 'vision-fixture' }] })); return;
  }
  const chunks: Buffer[] = []; for await (const chunk of request) chunks.push(Buffer.from(chunk));
  const body = Buffer.concat(chunks), text = body.toString(), parsed = JSON.parse(text) as { model: string; tools?: { name?: string; function?: { name: string } }[]; messages?: { role: string }[]; input?: { type?: string }[] };
  calls.push({ body, path: request.url!, route: String(request.headers['x-opencode-session'] ?? ''), agent: String(request.headers['user-agent'] ?? ''), authorization: String(request.headers.authorization ?? '') });
  const names = (parsed.tools ?? []).map(tool => tool.name ?? tool.function?.name);
  const hasResults = parsed.messages?.some(message => message.role === 'tool') || parsed.input?.some(item => item.type === 'function_call_output');
  let tools: ToolCall[] = [];
  if (names.includes('submit_session_title')) tools = [{ name: 'submit_session_title', arguments: { title: 'Fixture title' } }];
  else if (!hasResults && names.includes('delegate_task') && text.includes('routing-children')) tools = ['a', 'b'].map(child => ({ name: 'delegate_task', arguments: { title: `Inspect ${child}`, task: `routing-inspect-child-${child}`, context: `Inspect ${image}`, expected_output: 'One sentence' } }));
  else if (!hasResults && names.includes('inspect_images') && text.includes('routing-inspect')) tools = [{ name: 'inspect_images', arguments: { image_paths: [image], goal: 'Describe this fixture' } }];
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  response.end(sse(request.url!.endsWith('/responses'), parsed.model, tools));
});

class Host {
  readonly home: string;
  child: ChildProcess | undefined;
  bootstrap = ''; token = '';
  constructor(readonly mode: 'personal' | 'enterprise', readonly port: number) { this.home = join(root, mode); }
  async start() {
    await mkdir(this.home, { recursive: true, mode: 0o700 });
    await mkdir(join(this.home, 'os-home'), { recursive: true, mode: 0o700 });
    await writeFile(join(this.home, 'host.toml'), `version = "0.27.0"\nmode = "${this.mode}"\n${this.mode === 'enterprise' ? `[enterprise]\ncenter_url = "${origin}"\n` : ''}[host_access]\nport = ${this.port}\nremote_enabled = false\nscheme = "http"\nserver_names = []\n`, { mode: 0o600 });
    this.child = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', this.home], { env: { ...process.env, HOME: join(this.home, 'os-home') }, stdio: 'ignore' });
    await eventually(async () => {
      if (this.child!.exitCode !== null) throw new Error(`Host exited: ${this.child!.exitCode}`);
      try { const d = JSON.parse(await readFile(join(this.home, 'run/runtime.json'), 'utf8')) as { pid: number; access_token: string }; if (d.pid === this.child!.pid) this.bootstrap = d.access_token; } catch { /* 等待当前实例发现文件 */ }
      return Boolean(this.bootstrap);
    });
    if (this.mode === 'enterprise') {
      const response = await this.request('/auth/login', { method: 'enterprise', username: 'admin', password: '123456', native: true });
      expect(response.status).toBe(200); this.token = ((await response.json()) as { token: string }).token;
      await this.command('refresh_model_source');
    } else this.token = this.bootstrap;
  }
  async stop() {
    if (!this.child || this.child.exitCode !== null) return;
    const stopped = once(this.child, 'exit'); this.child.kill('SIGINT');
    await Promise.race([stopped, delay(10000).then(() => { throw new Error('Host stop timeout'); })]);
    this.bootstrap = ''; this.token = '';
  }
  request(path: string, body: object) {
    return fetch(`http://127.0.0.1:${this.port}${path}`, { method: 'POST', headers: { authorization: `Bearer ${this.token || this.bootstrap}`, 'x-ez-host-bootstrap': this.bootstrap, 'x-ez-client-version': '0.32.0', 'x-ez-min-compatible-version': '0.32.0', 'content-type': 'application/json' }, body: JSON.stringify(body) });
  }
  async command<T>(type: string, payload: object = {}): Promise<T> {
    const response = await this.request('/commands', { request_id: randomUUID(), command: { scope: 'runtime', payload: { type, payload } } });
    const result = await response.json() as { result: { payload: { payload: T } }; error?: unknown };
    expect(response.status, JSON.stringify(result)).toBe(200); return result.result.payload.payload;
  }
  async session() { return (await this.command<{ session: Session }>('create_session', { title: 'Routing fixture' })).session.session_id; }
  async run(session: string, message: string) {
    const started = await this.command<{ run: { run_id: string } }>('submit_input', { session_id: session, message, variant: 'build' });
    await eventually(async () => {
      const result = await this.command<{ run: { status: string; error?: unknown } }>('get_run', { session_id: session, run_id: started.run.run_id });
      if (['failed', 'cancelled'].includes(result.run.status)) throw new Error(JSON.stringify(result.run));
      return result.run.status === 'completed';
    });
  }
}
async function eventually(check: () => Promise<boolean>) {
  for (let i = 0; i < 200; i++) { if (await check()) return; await delay(50); }
  throw new Error('isolated scenario timeout');
}
const personal = new Host('personal', scenario.local_port), enterprise = new Host('enterprise', scenario.enterprise_port);
afterAll(async () => {
  await writeFile(join(root, 'captured-requests.json'), JSON.stringify(calls.map(call => ({ ...call, body: call.body.toString() }))));
  await personal.stop(); await enterprise.stop(); await app?.close();
  if (source.isInitialized) await source.destroy(); await pool.end();
  upstream.closeAllConnections(); if (upstream.listening) await new Promise<void>(done => upstream.close(() => done()));
});

it('正式双协议直连与企业下发代理使用同一本机方言，覆盖 owner、识图、子任务、标题与恢复', async () => {
  expect((await pool.query("SELECT host(inet_server_addr()) AS host, inet_server_port() AS port, current_database() AS db, current_user AS role, current_setting('data_directory') AS dir")).rows[0])
    .toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: 'f05_inference', role: 'f05_inference', dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT count(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  await upgradeDatabase(pool); await source.initialize();
  // 独立的一像素 PNG，仅供 Host 正式图片预处理读取。
  await writeFile(image, Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNoaGj4DwAFhAKAjM1mJgAAAABJRU5ErkJggg==', 'base64'));
  await new Promise<void>(done => upstream.listen(scenario.upstream_port, '127.0.0.1', done));
  app = await createHttpApp({ source, origin, snapshotRoot: join(root, 'snapshots') }); await app.listen(scenario.center_port, '127.0.0.1');
  const login = await app.inject({ method: 'POST', url: '/api/auth/login', payload: { username: 'admin', password: '123456' } });
  expect(login.statusCode).toBe(200); const authenticated = login.json<{ token: string; llm_key: string }>(); const admin = authenticated.token;
  const center = async <T>(method: 'GET' | 'POST' | 'PUT', path: string, payload?: object, status = 200): Promise<T> => {
    const response = await app!.inject({ method, url: `/api/${path}`, headers: { authorization: `Bearer ${admin}` }, ...(payload ? { payload } : {}) });
    expect(response.statusCode, response.body).toBe(status); return response.json<T>();
  };
  await personal.start();
  for (const preference of ['chat_completions', 'responses'] as const) {
    const connection = { display_name: `OpenCode ${preference}`, provider_type: 'opencode', endpoint: `http://127.0.0.1:${scenario.upstream_port}/inference/go/openai/v1`, models_path: '/inference/go/v1/models', protocol_preference: preference, discovery_format: 'openai', server_history_enabled: false, server_history_support_override: null };
    const parameters = { ...emptyParameters(), context_window_tokens: { state: 'known', value: 8192 }, max_output_tokens: { state: 'known', value: 1024 }, streaming: 'supported', image_input: 'unsupported', tool_calls: 'supported', tool_choice: { auto: 'supported', none: 'supported', required: 'supported', named: 'supported' }, reasoning: 'unsupported', reasoning_mode: 'unsupported', tool_image_projection: 'unsupported' };
    const local = await personal.command<{ provider_instance_id: string }>('create_provider', { connection, credential: { mode: 'replace', value: 'local-fixture-key' } });
    const remote = await center<{ provider_instance_id: string }>('POST', 'model-providers', { connection: { ...connection, responses_transport: 'http' }, credential: { mode: 'replace', value: 'upstream-fixture-key' } }, 201);
    const localSelection = { provider_instance_id: local.provider_instance_id, model_id: 'main-fixture' };
    const remoteSelection = { provider_instance_id: remote.provider_instance_id, model_id: 'main-fixture' };
    for (const model of ['main-fixture', 'vision-fixture']) {
      const p = model === 'vision-fixture' ? { ...parameters, image_input: 'supported' } : parameters;
      await personal.command('save_model_fixed_config', { selection: { ...localSelection, model_id: model }, origin: 'manual', parameters: p });
      await center('PUT', 'model-configuration', { ...remoteSelection, model_id: model, origin: 'manual', parameters: { ...p, image_input: 'supported' } });
    }
    await personal.command('set_default_model', { selection: localSelection });
    await personal.command('set_auxiliary_vision_model', { selection: { ...localSelection, model_id: 'vision-fixture' } });
    await center('PUT', 'model-settings', { default_model: remoteSelection });
    if (!enterprise.child || enterprise.child.exitCode !== null) await enterprise.start();
    await enterprise.command('refresh_model_source');
    const remoteSettings = await enterprise.command<{ default_model: Selection }>('get_model_settings');
    expect(remoteSettings.default_model).toEqual({ provider_instance_id: "managed", model_id: remoteSelection.model_id });
    const validationBodies: Buffer[] = [];
    for (const [host, selection] of [[personal, localSelection], [enterprise, remoteSettings.default_model]] as const) {
      const start = calls.length;
      const validation = await host.command<{ outcome: { status: string } }>('validate_model_connection', { selection });
      expect(validation.outcome.status).toBe('succeeded'); validationBodies.push(calls.at(-1)!.body);
      expect(calls.at(-1)!.route).toMatch(/^connection-test_/);
      const session = await host.session();
      await host.command('set_session_approval_mode', { session_id: session, approval_mode: 'auto' });
      await host.run(session, 'routing-inspect');
      const owned = calls.slice(start + 1);
      expect(owned.length).toBe(host.mode === 'personal' ? 3 : 1); // 主模型工具轮次 → 识图模型 → 主模型继续。
      expect(owned.every(call => call.route === routing(session))).toBe(true);
      expect(owned.some(call => JSON.parse(call.body.toString()).model === 'vision-fixture')).toBe(host.mode === 'personal');
      await host.command('generate_session_title', { session_id: session });
      await eventually(async () => (await host.command<{ session: Session }>('get_session', { session_id: session })).session.title === 'Fixture title');
      expect(calls.at(-1)!.route).toBe(routing(session));
      await host.run(session, 'second turn');
      await host.command('compact_session', { session_id: session, operation_id: randomUUID(), expected_generation: 1 });
      expect(calls.at(-1)!.route).toBe(routing(session));
      await host.command('clear_session', { session_id: session, operation_id: randomUUID(), expected_generation: 2 });
      await host.run(session, 'after clear'); expect(calls.at(-1)!.route).toBe(routing(session));
      await host.stop(); await host.start();
      await host.run(session, 'after restart'); expect(calls.at(-1)!.route).toBe(routing(session));
      const childSession = await host.session();
      await host.command('set_session_approval_mode', { session_id: childSession, approval_mode: 'auto' });
      const childStart = calls.length; await host.run(childSession, 'routing-children');
      const childCalls = calls.slice(childStart);
      const routes = new Set(childCalls.map(call => call.route)); expect(routes.size).toBe(3);
      for (const route of routes) {
        expect(route).toMatch(/^[a-f0-9]{64}$/);
        if (route !== routing(childSession)) expect(childCalls.filter(call => call.route === route).map(call => JSON.parse(call.body.toString()).model)).toEqual(host.mode === 'personal' ? ['main-fixture', 'vision-fixture', 'main-fixture'] : ['main-fixture']);
      }
      expect(childCalls[0]!.route).toBe(routing(childSession)); expect(childCalls.at(-1)!.route).toBe(routing(childSession));
    }
    expect(validationBodies[0]).toEqual(validationBodies[1]);
    // 企业现有默认模型也支持图片；个人客户端可显式使用文本配置并通过另一配置调用同一模型识图。
    // 这样验证辅助入口经过真实中心代理，同时不扩展只下发单模型的企业契约。
    const proxySelections: Selection[] = [];
    for (const vision of [false, true]) {
      const proxyProvider = await personal.command<{ provider_instance_id: string }>('create_provider', {
        connection: { ...connection, display_name: `Proxy ${vision ? 'vision' : 'text'}`, endpoint: `${origin}/api/llm/providers/${remote.provider_instance_id}/v1` },
        credential: { mode: 'replace', value: authenticated.llm_key },
      });
      const selection = { provider_instance_id: proxyProvider.provider_instance_id, model_id: 'main-fixture' };
      await personal.command('save_model_fixed_config', { selection, origin: 'manual', parameters: { ...parameters, image_input: vision ? 'supported' : 'unsupported' } });
      proxySelections.push(selection);
    }
    await personal.command('set_default_model', { selection: proxySelections[0] });
    await personal.command('set_auxiliary_vision_model', { selection: proxySelections[1] });
    const proxySession = await personal.session();
    await personal.command('set_session_approval_mode', { session_id: proxySession, approval_mode: 'auto' });
    const proxyStart = calls.length; await personal.run(proxySession, 'routing-inspect');
    const auxiliaryProxyCalls = calls.slice(proxyStart); expect(auxiliaryProxyCalls.length).toBe(3);
    expect(auxiliaryProxyCalls.every(call => call.route === routing(proxySession) && call.authorization === 'Bearer upstream-fixture-key')).toBe(true);
    expect(auxiliaryProxyCalls.some(call => call.body.includes(Buffer.from('data:image/')))).toBe(true);

  }
  expect(calls.every(call => call.agent === 'ez-assistant/0.32.0' && Boolean(call.route))).toBe(true);
  expect(calls.some(call => call.authorization === 'Bearer local-fixture-key')).toBe(true);
  expect(calls.some(call => call.authorization === 'Bearer upstream-fixture-key')).toBe(true);
  expect(calls.every(call => /\/(?:responses|chat\/completions)$/.test(call.path))).toBe(true);
  const counts = (await source.query('SELECT (SELECT count(*)::integer FROM providers) AS providers, (SELECT count(*)::integer FROM model_fixed_configs) AS fixed, (SELECT count(*)::integer FROM model_settings) AS settings, (SELECT count(*)::integer FROM llm_calls) AS calls'))[0];
  expect(counts.providers).toBe(2); expect(counts.fixed).toBe(4); expect(counts.settings).toBe(1);
  expect(counts.calls).toBe(calls.filter(call => call.authorization === 'Bearer upstream-fixture-key').length);
  const stored = await source.query('SELECT connection, api_key FROM providers');
  expect(stored.every((row: { connection: { provider_type: string }; api_key: string }) => row.connection.provider_type === 'opencode' && row.api_key === 'upstream-fixture-key')).toBe(true);
}, 120000);
