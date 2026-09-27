import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from '@ez-assistant/protocol';
import { expect, test } from '@playwright/test';
import { createServer } from 'node:http';

// 使用全局夹具新建的隔离 Host；真实 HTTP Adapter + SSE + 页面，不访问外部模型。
test('F01 retries recover, survive reload, stop on cancellation and reject stream replay', async ({ page }) => {
  test.setTimeout(90_000);
  const counts = new Map<string, number>();
  const server = createServer(async (request, response) => {
    if (request.method === 'GET') {
      response.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ object: 'list', data: [{ id: 'retry-fixture', object: 'model' }] }));
      return;
    }
    let raw = '';
    for await (const chunk of request) raw += chunk;
    const marker = raw.match(/F01_(RECOVER|AUTH|EXHAUST|CANCEL|STREAM)/)?.[0] ?? 'UNKNOWN';
    const attempt = (counts.get(marker) ?? 0) + 1;
    counts.set(marker, attempt);
    if (marker === 'F01_AUTH') {
      response.writeHead(401, { 'Content-Type': 'application/json' }).end(JSON.stringify({ error: { message: 'private credential detail' } }));
      return;
    }
    if (marker === 'F01_EXHAUST' || marker === 'F01_CANCEL' || (marker === 'F01_RECOVER' && attempt === 1)) {
      response.writeHead(503, { 'Content-Type': 'application/json', ...(marker === 'F01_EXHAUST' ? {} : { 'Retry-After': '5' }) }).end(JSON.stringify({ error: { message: 'private overload detail' } }));
      return;
    }
    response.writeHead(200, { 'Content-Type': 'text/event-stream' });
    const chunk = (delta: object) => response.write(`data: ${JSON.stringify({ id: `retry-${marker}`, model: 'retry-fixture', choices: [{ index: 0, delta }] })}\n\n`);
    chunk({ role: 'assistant' });
    chunk({ content: marker === 'F01_STREAM' ? 'partial response' : 'F01 恢复成功' });
    if (marker === 'F01_STREAM') {
      setTimeout(() => response.destroy(), 150);
      return;
    }
    response.end(`data: ${JSON.stringify({ id: `retry-${marker}`, model: 'retry-fixture', choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 120, completion_tokens: 30, total_tokens: 150 } })}\n\ndata: [DONE]\n\n`);
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('fixture port missing');
  try {
    const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
    const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, 'Content-Type': 'application/json' };
    async function command<T extends RuntimeCommand['type']>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>['payload']> {
      const response = await fetch(`${host.base_url}/commands`, { method: 'POST', headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }) });
      const body = await response.json();
      expect(response.status, JSON.stringify(body)).toBe(200);
      return body.result.payload.payload;
    }
    const provider = await command('create_provider', { connection: { display_name: 'F01 fixture', provider_type: 'local', endpoint: `http://127.0.0.1:${address.port}/v1`, protocol_preference: 'chat_completions', models_path: '/v1/models', discovery_format: 'openai' }, credential: { mode: 'replace', value: 'fixture-only' } });
    await command('refresh_provider_models', { provider_instance_id: provider.provider_instance_id });
    const selection = { provider_instance_id: provider.provider_instance_id, model_id: 'retry-fixture' };
    await command('save_model_fixed_config', { selection, parameters: {
      context_window_tokens: { state: 'known', value: 16384 }, max_output_tokens: { state: 'known', value: 4096 },
      max_input_tokens: { state: 'unknown' }, reasoning_max_input_tokens: { state: 'unknown' }, reasoning_max_output_tokens: { state: 'unknown' },
      streaming: 'supported', image_input: 'unsupported', tool_calls: 'supported', reasoning: 'unsupported',
      tool_choice: { auto: 'supported', none: 'supported', required: 'supported', named: 'supported' },
      tool_image_projection: 'unsupported', reasoning_mode: 'unsupported', reasoning_efforts: {}, default_reasoning_effort: null,
    } });
    const app = (await command('get_application_snapshot')).snapshot.value;
    const sessions = new Map<string, string>();
    for (const scenario of ['RECOVER', 'AUTH', 'EXHAUST', 'CANCEL', 'STREAM']) {
      const created = await command('create_session', { title: `F01 ${scenario}`, model_selection: selection, workspace_id: app.workspaces[0].workspace_id });
      sessions.set(scenario, created.session.session_id);
    }
    const issued = await fetch(`${host.base_url}/auth/login`, { method: 'POST', headers, body: JSON.stringify({ method: 'desktop' }) });
    const { token } = await issued.json();
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
    for (const scenario of ['RECOVER', 'AUTH', 'EXHAUST', 'CANCEL', 'STREAM']) {
      await page.getByText(`F01 ${scenario}`, { exact: true }).first().click();
      const session_id = sessions.get(scenario)!;
      const submitted = await command('submit_input', { session_id, message: `F01_${scenario}`, mode: 'normal', variant: 'build', attachment_ids: [], quotes: [] });
      const run_id = submitted.run.run_id;
      if (scenario === 'RECOVER' || scenario === 'CANCEL') {
        await expect(page.getByRole('status').filter({ hasText: /等待 5 秒后进行第 2 次尝试/ })).toBeVisible();
        const run = (await command('get_run', { session_id, run_id })).run;
        expect(run.model_request).toEqual({ type: 'retry_waiting', next_attempt: 2, delay_ms: 5000 });
        if (scenario === 'RECOVER') {
          await page.reload();
          await page.getByText('F01 RECOVER', { exact: true }).first().click();
          await expect(page.getByRole('status').filter({ hasText: /等待 5 秒后进行第 2 次尝试/ })).toBeVisible();
          await page.screenshot({ path: test.info().outputPath('retry-after-reload.png') });
        } else {
          await page.getByRole('button', { name: '中断当前轮次', exact: true }).click();
        }
      }
      const expected_status = scenario === 'RECOVER' ? 'completed' : scenario === 'CANCEL' ? 'cancelled' : 'failed';
      await expect.poll(async () => (await command('get_run', { session_id, run_id })).run.status, { timeout: 45000 }).toBe(expected_status);
      expect(counts.get(`F01_${scenario}`)).toBe(scenario === 'RECOVER' ? 2 : scenario === 'EXHAUST' ? 6 : 1);
      if (scenario === 'RECOVER') await expect(page.getByText('F01 恢复成功', { exact: true })).toBeVisible();
      if (scenario === 'AUTH') await expect(page.getByRole('alert')).toContainText('模型认证失败');
      if (scenario === 'EXHAUST') {
        await expect(page.getByRole('alert')).toContainText('已尝试 6 次');
        await page.reload();
        await page.getByText('F01 EXHAUST', { exact: true }).first().click();
        await expect(page.getByRole('alert')).toContainText('已尝试 6 次');
      }
      if (scenario === 'STREAM') await expect(page.getByRole('alert')).toContainText('已停止自动重试');
      const view = (await command('get_session_view', { session_id })).snapshot.value;
      expect(view.conversation.items.filter(item => item.type === 'user')).toHaveLength(1);
      expect(view.active_run).toBeNull();
      expect(await page.locator('body').innerText()).not.toContain('private overload');
    }
  } finally {
    server.closeAllConnections();
    await new Promise<void>(resolve => server.close(() => resolve()));
  }
});
