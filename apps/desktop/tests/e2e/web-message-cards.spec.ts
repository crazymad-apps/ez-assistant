import { test, expect, type Route } from '@playwright/test';
import { compatibilityHeaders } from '@ez-assistant/protocol';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const host = () => JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
async function command(type: string, payload: object) {
  const response = await fetch(`${host().base_url}/commands`, {
    method: 'POST',
    headers: {
      ...compatibilityHeaders(),
      Authorization: `Bearer ${host().access_token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      request_id: crypto.randomUUID(),
      command: { scope: 'runtime', payload: { type, payload } },
    }),
  });
  expect(response.status).toBe(200);
  return (await response.json()).result.payload.payload;
}

test('声明插件发布卡片，保存快照后刷新恢复且不重跑工具', async ({ page }) => {
  const archive = resolve(process.env.EZ_ASSISTANT_E2E_TEST_ROOT!, 'generic-ui.zip');
  await promisify(execFile)('python3', [
    '-c',
    `import json,sys,zipfile
from pathlib import Path
base=json.loads(Path(sys.argv[1]).read_text())['base']
with zipfile.ZipFile(sys.argv[3],'w') as z:
 z.writestr('plugin.json',json.dumps(base))
 z.writestr('resources/counter.html',Path(sys.argv[2]).read_text())`,
    resolve('../../packages/assistant-protocol/fixtures/plugin-ui.json'),
    resolve('../runtime-host/tests/fixtures/mcp_ui_app.html'),
    archive,
  ]);
  const bytes = await readFile(archive);
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host().access_token}` };
  const inspected = await fetch(`${host().base_url}/plugins/inspect`, {
    method: 'POST',
    headers: { ...headers, 'x-ez-plugin-format': 'zip' },
    body: bytes,
  });
  expect(inspected.status).toBe(200);
  const selection = await inspected.json();
  const key = crypto.randomUUID();
  await command('begin_plugin_install', { key, packages: [selection] });
  const uploaded = await fetch(`${host().base_url}/plugins/operations/${key}/package`, {
    method: 'POST',
    headers,
    body: bytes,
  });
  expect(uploaded.ok).toBeTruthy();
  await expect.poll(async () => (await command('get_plugin_operation', { key })).phase).toBe('succeeded');
  const catalog = await command('list_plugins', {});
  expect(catalog.plugins.find((plugin: { name: string }) => plugin.name === 'ui-fixture').ui.entries[0].id).toBe(
    'counter',
  );
  const created = await command('create_session', { title: 'F03 card test' });
  const session_id = created.session.session_id;
  const started = await command('submit_input', {
    session_id,
    message: 'F03_CARD',
    variant: 'build',
    mode: 'normal',
    attachment_ids: [],
    quotes: [],
  });
  let run_id = started.run?.run_id as string | undefined;
  await expect
    .poll(async () => {
      const view = await command('get_session_view', { session_id });
      const run = view.snapshot.value.runs.find((run: { input_id: string }) => run.input_id === started.input_id);
      if (run) run_id = run.run_id;
      return run?.status;
    })
    .toBe('completed');
  const owner = { type: 'main_session', session_id };
  const query = { owner, run_ids: [run_id], message_ids: [], after: null, limit: 100 };
  const published = await command('list_message_cards', query);
  expect(published.cards).toHaveLength(2);
  const login = await fetch(`${host().base_url}/auth/login`, {
    method: 'POST',
    headers: { ...headers, 'Content-Type': 'application/json' },
    body: JSON.stringify({ method: 'desktop' }),
  });
  const { token } = await login.json();
  await page.goto(`${host().base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole('button', { name: 'F03 card test', exact: true }).click();
  const cards = page.getByRole('region', { name: '消息卡片：Counter' });
  await expect(cards).toHaveCount(2);
  const card = cards.first();
  const frame = (index: number) =>
    cards.nth(index).frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  await expect(frame(0).locator('#state')).toHaveText('累计 1');
  await expect(frame(1).locator('#state')).toHaveText('累计 7');
  const reply = page.getByText('离线回复：F03_CARD', { exact: true });
  const copy = page.getByRole('button', { name: '复制助手正文', exact: true });
  await expect(reply).toBeVisible();
  await expect(copy).toBeVisible();
  const [reply_bounds, first_bounds, second_bounds, copy_bounds] = await Promise.all([
    reply.boundingBox(),
    card.boundingBox(),
    cards.last().boundingBox(),
    copy.boundingBox(),
  ]);
  expect(reply_bounds!.y + reply_bounds!.height).toBeLessThanOrEqual(first_bounds!.y);
  expect(first_bounds!.y + first_bounds!.height).toBeLessThanOrEqual(second_bounds!.y);
  expect(second_bounds!.y + second_bounds!.height).toBeLessThanOrEqual(copy_bounds!.y);
  const initial_viewport = page.viewportSize()!;
  const iframe = card.locator('iframe[title="应用界面"]');
  const body = frame(0).locator('body');
  const report_content_height = (height: number) => body.evaluate((element, min_height) => {
    element.style.margin = '0';
    element.style.display = 'flow-root';
    element.style.minHeight = `${min_height}px`;
    window.parent.postMessage({
      jsonrpc: '2.0',
      method: 'ui/notifications/size-changed',
      params: { height: element.getBoundingClientRect().height },
    }, '*');
  }, height);
  const scroll_range = () => body.evaluate((element) => {
    const scroller = element.ownerDocument.scrollingElement!;
    return scroller.scrollHeight - scroller.clientHeight;
  });

  await page.setViewportSize({ ...initial_viewport, height: 1300 });
  await report_content_height(900);
  await expect(iframe).toHaveCSS('height', '600px');
  await page.setViewportSize({ ...initial_viewport, height: 700 });
  await expect(iframe).toHaveCSS('height', '600px');
  expect(await scroll_range()).toBeGreaterThan(0);
  await report_content_height(280);
  await expect(iframe).toHaveCSS('height', '280px');
  expect(await scroll_range()).toBe(0);
  await page.setViewportSize(initial_viewport);
  await frame(0).getByRole('button', { name: '增加', exact: true }).click();
  await frame(0).getByRole('button', { name: '保存', exact: true }).click();
  await expect(frame(0).locator('#state')).toHaveText('已保存 2');

  const old_height = await card.evaluate((node) => node.getBoundingClientRect().height);
  const mount_gate = Promise.withResolvers<void>();
  const endpoint = `${host().base_url}/commands`;
  const pause_mount = async (route: Route) => {
    const request = route.request().postDataJSON()?.command?.payload;
    if (request?.type === 'mount_message_card' && request.payload.card_id === published.cards[0].card_id) {
      await mount_gate.promise;
    }
    await route.continue();
  };
  await page.route(endpoint, pause_mount);
  try {
    await card.getByRole('button', { name: '重新加载卡片', exact: true }).click();
    await expect(card.getByRole('status')).toHaveText('正在加载卡片…');
    expect(await card.evaluate((node) => node.getBoundingClientRect().height)).toBe(old_height);
  } finally {
    mount_gate.resolve();
  }
  await expect(frame(0).locator('#state')).toHaveText('累计 2');
  await expect(iframe).toHaveCSS('height', '280px');
  await report_content_height(360);
  await expect(iframe).toHaveCSS('height', '360px');
  await page.unroute(endpoint, pause_mount);

  await page.reload();
  await page.getByRole('button', { name: 'F03 card test', exact: true }).click();
  await expect(frame(0).locator('#state')).toHaveText('累计 2');
  await expect(frame(1).locator('#state')).toHaveText('累计 7');
  const restored = await command('list_message_cards', query);
  expect(restored.cards.map((item: { card_id: string }) => item.card_id)).toEqual(
    published.cards.map((item: { card_id: string }) => item.card_id),
  );
  expect(restored.cards.map((item: { snapshot_revision: number }) => item.snapshot_revision)).toEqual([1, 0]);
});

test('标准 MCP 工具结果自动发布卡片，私有结果交给 UI 且刷新不重跑', async ({ page }) => {
  const created = await command('create_session', { title: 'F03 MCP card test' });
  const session_id = created.session.session_id;
  await command('set_session_approval_mode', { session_id, approval_mode: 'auto' });
  await expect
    .poll(async () => {
      const listed = await command('list_mcp_ui_resources', {
        target: { source: { type: 'user' }, server_key: 'ui_fixture' },
      });
      return listed.resources.length;
    })
    .toBe(1);
  const started = await command('submit_input', {
    session_id,
    message: 'F03_MCP_CARD',
    variant: 'build',
    mode: 'normal',
    attachment_ids: [],
    quotes: [],
  });
  let run_id: string | undefined;
  await expect
    .poll(async () => {
      const view = await command('get_session_view', { session_id });
      const run = view.snapshot.value.runs.find((run: { input_id: string }) => run.input_id === started.input_id);
      run_id = run?.run_id;
      return run?.status;
    })
    .toBe('completed');
  const query = {
    owner: { type: 'main_session', session_id },
    run_ids: [run_id],
    message_ids: [],
    after: null,
    limit: 100,
  };
  const published = await command('list_message_cards', query);
  expect(published.cards).toHaveLength(1);
  const issued = await fetch(`${host().base_url}/auth/login`, {
    method: 'POST',
    headers: {
      ...compatibilityHeaders(),
      Authorization: `Bearer ${host().access_token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ method: 'desktop' }),
  });
  const { token } = await issued.json();
  await page.goto(`${host().base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole('button', { name: 'F03 MCP card test', exact: true }).click();
  const app = page.frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  await expect(app.locator('body')).toHaveAttribute('data-private-result', 'ui-only');
  await expect(app.locator('body')).toHaveAttribute('data-input', 'card input');
  await expect(app.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
  const app_tool = app.getByRole('button', { name: '调用应用工具', exact: true });
  const app_approval = page.getByRole('dialog', { name: '应用工具授权' });
  await app_tool.click();
  await expect(app.locator('#tool-result')).toHaveText(/^App calls \d+$/);
  await expect(app_approval).toHaveCount(0);
  await expect(app.locator('#tool-result')).toHaveAttribute('data-private', 'tool-private');
  const first_result = await app.locator('#tool-result').textContent();
  await app_tool.press('Enter');
  await expect(app.locator('#tool-result')).not.toHaveText(first_result!);
  await expect(app.locator('#tool-result')).toHaveText(/^App calls \d+$/);
  await expect(app_approval).toHaveCount(0);

  await app_tool.evaluate((button: HTMLButtonElement) => button.click());
  await expect(app_approval).toContainText('当前会话');
  await expect(app_approval).toContainText('app_only');
  await app_approval.getByRole('button', { name: '拒绝', exact: true }).click();
  await expect(app.locator('#tool-result')).toHaveText('工具调用未获授权。');
  await page.reload();
  await page.getByRole('button', { name: 'F03 MCP card test', exact: true }).click();
  await expect(app.locator('body')).toHaveAttribute('data-private-result', 'ui-only');
  const restored = await command('list_message_cards', query);
  expect(restored.cards.map((card: { card_id: string }) => card.card_id)).toEqual(
    published.cards.map((card: { card_id: string }) => card.card_id),
  );
  const view = await command('get_session_view', { session_id });
  expect(view.snapshot.value.runs).toHaveLength(1);
});
