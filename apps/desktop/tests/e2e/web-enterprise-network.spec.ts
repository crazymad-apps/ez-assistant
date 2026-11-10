import { expect, test } from '@playwright/test';
import { createServer } from 'node:http';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { compatibilityHeaders } from '@ez-assistant/protocol';

// 正式企业 Host + HTTP 模型流 + 浏览器。故障仅注入 loopback 假中心；不操作系统网络。
for (const scenario of ['reset', 'stall', 'recover', 'host_disconnect', 'center_only'] as const) {
  test(`enterprise network after tool: ${scenario}`, async ({ page }, info) => {
    test.setTimeout(100_000);
    const center_id = '01234567-89ab-4cde-8f01-23456789abcd';
    const organization_id = '01234567-89ab-4cde-8f01-23456789abce';
    const organization = { kind: 'organization', organization_id, name: '测试组织', enabled: true, member_role: 'member' };
    const identity = { center_id, legacy_organization_id: organization_id, scope: organization,
      expires_at: new Date(Date.now() + 60 * 60 * 1000).toISOString(), user: { id: 1, username: 'fixture', display_name: 'Fixture', role: 'user', enabled: true, is_super_admin: false } };
    let unavailable = false, model_attempts = 0, response_sequence = 0;
    let release!: () => void;
    const held = new Promise<void>(resolve => { release = resolve; });
    const center = createServer(async (request, response) => {
      let raw = '';
      for await (const chunk of request) raw += chunk;
      const body = raw ? JSON.parse(raw) : {};
      const json = (value: unknown, status = 200) => { response.writeHead(status, { 'Content-Type': 'application/json' }).end(JSON.stringify(value)); };
      if (request.url === '/api/info') return json({ center_id, protocol_version: 6, min_protocol_version: 6, capabilities: ['managed_models', 'llm_proxy'] });
      if (request.url === '/api/auth/login') return json({ ...identity, scope: { kind: 'unselected' }, token: 'pending.fixture.token' });
      if (request.url === '/api/auth/scopes?entry=client') return json([organization]);
      if (request.url === '/api/auth/scope' && body.organization_id === organization_id)
        return json({ ...identity, token: 'selected.fixture.token', llm_key: `cl_${'b'.repeat(64)}` });
      if (request.url === '/api/auth/me') return json(unavailable ? {} : identity, unavailable ? 503 : 200);
      if (request.url === '/api/runtime/model-configuration') return json({
        center_id, organization_id, allow_member_selection: false,
        default_model: { provider_instance_id: center_id, model_id: 'fixture' },
        providers: [{
          provider_instance_id: center_id,
          endpoint: `http://127.0.0.1:${(center.address() as { port: number }).port}/api/llm/providers/${center_id}/v1`,
          provider_type: 'openai', provider_display_name: 'Network fixture', protocol: 'open_ai_chat_completions',
          server_history_enabled: false, server_history_support: 'unknown',
          models: [{
            state: 'ready', model_id: 'fixture', server_history_binding: null,
            parameters: { context_window_tokens: { state: 'known', value: 16384 }, max_output_tokens: { state: 'known', value: 1024 },
              max_input_tokens: { state: 'unknown' }, reasoning_max_input_tokens: { state: 'unknown' }, reasoning_max_output_tokens: { state: 'unknown' },
              image_input: 'unsupported', reasoning: 'unsupported', tool_image_projection: 'unsupported', reasoning_mode: 'unsupported', reasoning_efforts: null, default_reasoning_effort: null,
              streaming: 'supported', tool_calls: 'supported', tool_choice: { auto: 'supported', none: 'supported', required: 'supported', named: 'supported' } },
          }],
        }],
      });
      if (request.url === '/api/runtime/agents') return json([]);
      if (!request.url?.endsWith('/chat/completions')) return json({});
      const has_tool = body.messages?.some((message: { role: string }) => message.role === 'tool');
      const response_id = `fixture-response-${++response_sequence}`;
      const chunk = (delta: object, finish_reason: string | null = null) => response.write(`data: ${JSON.stringify({ id: response_id, model: 'fixture', choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
      if (!has_tool) {
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        chunk({ role: 'assistant', tool_calls: [{ index: 0, id: 'fixture-tool', type: 'function', function: { name: 'write_file', arguments: JSON.stringify({ path: 'network-fixture.txt', content: 'isolated tool result' }) } }] });
        chunk({}, 'tool_calls');
        response.end('data: [DONE]\n\n');
        return;
      }
      model_attempts++;
      if (model_attempts === 1) await held;
      unavailable = !((scenario === 'recover' || scenario === 'host_disconnect') && model_attempts === 3);
      if (!unavailable || scenario === 'center_only') {
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        chunk({ content: '网络恢复，执行完成' });
        chunk({}, 'stop');
        response.end('data: [DONE]\n\n');
      } else if (scenario === 'stall') {
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        response.write(': connected\n\n'); // 已收响应头，没有任何模型输出，等待正式 Transport 空闲超时。
      } else {
        response.destroy(); // 请求真实失败，复用正式重试，不能用页面伪造 retry 事件。
      }
    });
    await new Promise<void>(resolve => center.listen(0, '127.0.0.1', resolve));
    const root = resolve('../../.runtime-test/network-browser');
    await mkdir(root, { recursive: true });
    const home = await mkdtemp(join(root, 'run-'));
    const workspace = await mkdtemp(join(root, 'workspace-'));
    let host: ChildProcess | undefined;
    let host_error = '';
    try {
      await mkdir(join(home, 'os-home'));
      const user_home = join(home, 'users', `${center_id}_1`);
      await mkdir(user_home, { recursive: true });
      await writeFile(join(user_home, 'config.toml'), "schema_version=1\n[runtime.model_transport]\nconnect_timeout_ms=1000\nrequest_timeout_ms=2500\n[runtime.model_retry]\nretry_on=['connection','timeout']\ndelays_ms=[1000,1000]\nmax_retry_after_ms=1000\n");
      const probe = createServer();
      await new Promise<void>(resolve => probe.listen(0, '127.0.0.1', resolve));
      const port = (probe.address() as { port: number }).port;
      await new Promise<void>(resolve => probe.close(() => resolve()));
      await writeFile(join(home, 'host.toml'), `version="0.27.0"\nmode="enterprise"\n[enterprise]\ncenter_url="http://127.0.0.1:${(center.address() as { port: number }).port}"\n[host_access]\nport=${port}\nremote_enabled=false\n`);
      host = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', home], { env: { ...process.env, HOME: join(home, 'os-home') }, stdio: ['ignore', 'ignore', 'pipe'] });
      host.stderr?.on('data', chunk => { host_error += chunk.toString(); });
      let address = '';
      await expect.poll(async () => {
        if (host?.exitCode !== null) throw new Error(`Host exited ${host?.exitCode}: ${host_error}`);
        try { address = JSON.parse(await readFile(join(home, 'run/runtime.json'), 'utf8')).address; return !!address; } catch { return false; }
      }, { timeout: 20_000, message: 'isolated Host readiness' }).toBe(true);
      const headers = { ...compatibilityHeaders(), 'Content-Type': 'application/json' };
      const login = await fetch(`${address}/auth/login`, { method: 'POST', headers, body: JSON.stringify({ method: 'enterprise', username: 'fixture', password: 'fixture', native: true }) });
      const pending = await login.json();
      expect(login.status).toBe(200);
      expect(pending.token).toBeNull();
      const selected = await fetch(`${address}/auth/login`, { method: 'POST', headers, body: JSON.stringify({ method: 'enterprise_select', selection_token: pending.organization_selection.selection_token, organization_id }) });
      expect(selected.status).toBe(200);
      const auth = await selected.json();
      const authorized = { ...headers, Authorization: `Bearer ${auth.token}` };
      const ready = await fetch(`${address}/runtime/ensure-ready`, { method: 'POST', headers: authorized });
      expect(ready.status).toBe(204);
      const command = async (type: string, payload: object = {}) => {
        const response = await fetch(`${address}/commands`, { method: 'POST', headers: authorized, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }) });
        const body = await response.json();
        expect(response.status, JSON.stringify(body)).toBe(200);
        return body.result.payload.payload;
      };
      await command('refresh_model_source');
      const { workspace: registered } = await command('register_workspace', { label: 'Isolated network fixture', primary_directory: workspace, additional_directories: [] });
      const { session } = await command('create_session', { title: '工具后网络故障', workspace_id: registered.workspace_id });
      // 记录浏览器收到的事件类型和实际 DOM 文字，避免 Store 测试代替页面验收。
      await page.addInitScript(() => {
        const trace = { events: [] as { type: string; attempt?: number; next_attempt?: number }[], labels: [] as string[], streams: 0, disconnect: false };
        Object.assign(window, { networkTrace: trace });
        const original = window.fetch.bind(window);
        window.fetch = async (...args) => {
          const response = await original(...args);
          const input = args[0];
          const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
          if (new URL(url, location.href).pathname !== '/events' || !response.body) return response;
          trace.streams++;
          let pending = '';
          const decoder = new TextDecoder();
          return new Response(response.body.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({ transform(chunk, output) {
            if (trace.disconnect) { trace.disconnect = false; output.error(new TypeError('isolated Host event stream interruption')); return; }
            pending += decoder.decode(chunk, { stream: true });
            const frames = pending.split('\n\n'); pending = frames.pop() ?? '';
            for (const frame of frames) {
              if (!frame.includes('event: runtime_event')) continue;
              const data = frame.split('\n').find(line => line.startsWith('data:'))?.slice(5);
              if (data) { const { event } = JSON.parse(data); trace.events.push({ type: event.type, attempt: event.attempt, next_attempt: event.next_attempt }); }
            }
            output.enqueue(chunk);
          } })), { status: response.status, headers: response.headers });
        };
        document.addEventListener('DOMContentLoaded', () => new MutationObserver(() => {
          const label = document.querySelector('[data-processing-turn] [role=status]')?.textContent;
          if (label && trace.labels.at(-1) !== label) trace.labels.push(label);
        }).observe(document.body, { childList: true, subtree: true, characterData: true }));
      });
      await page.goto(`${address}/#token=${encodeURIComponent(auth.token)}`);
      await page.getByRole('button', { name: '工具后网络故障', exact: true }).click();
      await command('submit_input', { session_id: session.session_id, message: '执行一次工具然后继续', mode: 'normal', variant: 'build', attachment_ids: [], quotes: [] });
      await expect.poll(() => model_attempts).toBe(1);
      await expect(page.getByRole('button', { name: /工具调用：/ })).toBeVisible();
      if (scenario === 'host_disconnect') await page.evaluate(() => { (window as unknown as { networkTrace: { disconnect: boolean } }).networkTrace.disconnect = true; });
      release();
      if (scenario === 'center_only') {
        await expect(page.getByText('网络恢复，执行完成', { exact: true })).toBeVisible();
        expect(model_attempts).toBe(1);
        expect((await command('get_session_view', { session_id: session.session_id })).snapshot.value.active_run).toBeNull();
        await page.getByRole('button', { name: '处理过程', exact: true }).click();
        await page.getByRole('button', { name: /工具调用：/ }).click({ timeout: 5_000 });
        const [detail_response] = await Promise.all([
          page.waitForResponse(response => response.url().endsWith('/commands')
            && response.request().postDataJSON()?.command?.payload?.type === 'get_tool_detail', { timeout: 5_000 }),
          page.getByRole('button', { name: 'write file 完成 network-fixture.txt', exact: true }).click({ timeout: 5_000 }),
        ]);
        expect(detail_response.status()).toBe(200);
        await page.getByRole('button', { name: '关闭工具详情', exact: true }).click();
        await expect(page.getByText('企业中心暂不可达，请稍后重试。', { exact: true })).toHaveCount(0);
        const notice = page.locator('[data-conversation-area]').getByRole('status').filter({ hasText: '企业中心暂不可达' });
        await expect(notice).toBeVisible({ timeout: 35_000 });
        expect(await notice.evaluate(node => node.closest('[data-output-scroll]') === null)).toBe(true);
        expect(await notice.evaluate(node => getComputedStyle(node).borderBottomWidth)).toBe('1px');
        await page.setViewportSize({ width: 900, height: 600 });
        await expect(notice).toBeVisible();
        await page.screenshot({ path: info.outputPath('center-unavailable.png') });
        expect((await command('get_session_view', { session_id: session.session_id })).snapshot.value.active_run).toBeNull();
        unavailable = false;
        await expect(notice).toHaveCount(0, { timeout: 35_000 });
        expect(model_attempts).toBe(1);
        expect(await page.evaluate(() => (window as unknown as { networkTrace: { streams: number } }).networkTrace.streams)).toBe(1);
        return;
      }
      if (scenario === 'host_disconnect') {
        await expect(page.locator('[data-conversation-area]').getByRole('button', { name: '重新连接', exact: true })).toBeVisible();
      } else {
        await expect(page.getByRole('status').filter({ hasText: '第 2 次尝试' })).toBeVisible({ timeout: 10_000 });
        await expect.poll(() => model_attempts).toBe(2);
        expect(await page.evaluate(() => (window as unknown as { networkTrace: { streams: number } }).networkTrace.streams)).toBe(1);
      }
      if (scenario === 'stall') {
        // HTTP 头不代表模型已恢复输出；整个等待响应阶段应能辨识这是第 2 次尝试。
        await expect(page.getByRole('status').filter({ hasText: '第 2 次尝试' })).toBeVisible();
        const view = await command('get_session_view', { session_id: session.session_id });
        expect(view.snapshot.value.active_run.model_request).toEqual({ type: 'requesting', attempt: 2 });
      }
      await expect(page.getByRole('status').filter({ hasText: ['recover', 'host_disconnect'].includes(scenario) ? '已完成' : '执行失败' })).toBeVisible({ timeout: 15_000 });
      expect(model_attempts).toBe(3);
      expect(await page.evaluate(() => (window as unknown as { networkTrace: { events: { type: string }[] } }).networkTrace.events.filter(event => event.type === 'tool_started').length)).toBe(1);
      await expect(page.getByRole('button', { name: '中断当前轮次', exact: true })).toHaveCount(0);
      await expect(page.getByRole('img', { name: '正在运行', exact: true })).toHaveCount(0);
      if (!['recover', 'host_disconnect'].includes(scenario)) await expect(page.getByRole('alert').filter({ hasText: '已尝试 3 次' })).toBeVisible();
      else await expect(page.getByText('网络恢复，执行完成', { exact: true })).toBeVisible();
      if (scenario === 'host_disconnect') {
        expect(await page.evaluate(() => (window as unknown as { networkTrace: { streams: number } }).networkTrace.streams)).toBeGreaterThan(1);
        await expect(page.locator('[data-conversation-area]').getByRole('button', { name: '重新连接', exact: true })).toHaveCount(0);
      }
    } finally {
      const trace = await page.evaluate(() => (window as unknown as { networkTrace?: unknown }).networkTrace).catch(() => null);
      console.log(JSON.stringify({ scenario, trace }));
      await info.attach('network-trace', { body: JSON.stringify(await page.evaluate(() => (window as unknown as { networkTrace?: unknown }).networkTrace).catch(() => null)), contentType: 'application/json' });
      await page.screenshot({ path: info.outputPath('network.png') }).catch(() => {});
      release();
      if (host_error) console.log(host_error);
      if (host && host.exitCode === null) { host.kill('SIGTERM'); await new Promise<void>(resolve => host!.once('exit', () => resolve())); }
      center.closeAllConnections();
      await new Promise<void>(resolve => center.close(() => resolve()));
      await rm(home, { recursive: true, force: true });
      await rm(workspace, { recursive: true, force: true });
    }
  });
}
