import { expect, test, type Page } from '@playwright/test';
import { compatibilityHeaders, type UiToolApproval } from '@ez-assistant/protocol';
import { readFile } from 'node:fs/promises';
import { createServer, type Server } from 'node:http';
import { resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

const host = () => JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
const headers = () => ({ ...compatibilityHeaders(), Authorization: `Bearer ${host().access_token}` });
async function command(type: string, payload?: object) {
  const response = await fetch(`${host().base_url}/commands`, {
    method: 'POST',
    headers: { ...headers(), 'Content-Type': 'application/json' },
    body: JSON.stringify({
      request_id: crypto.randomUUID(),
      command: { scope: 'runtime', payload: { type, ...(payload ? { payload } : {}) } },
    }),
  });
  expect(response.status, await response.clone().text()).toBe(200);
  return (await response.json()).result.payload.payload;
}
async function login(page: Page) {
  const response = await fetch(`${host().base_url}/auth/login`, {
    method: 'POST',
    headers: { ...headers(), 'Content-Type': 'application/json' },
    body: JSON.stringify({ method: 'desktop' }),
  });
  const { token } = await response.json();
  await page.goto(`${host().base_url}/#token=${encodeURIComponent(token)}`);
}

let external: Server;
let origin: string;
test.beforeAll(async () => {
  const html = await readFile('../runtime-host/tests/fixtures/mcp_ui_app.html', 'utf8');
  external = createServer((request, response) => {
    if (request.url === '/redirect') {
      response.writeHead(302, { location: origin.replace('127.0.0.1', 'localhost') + '/app' }).end();
      return;
    }
    response
      .writeHead(200, {
        'Content-Type': 'text/html',
        'Content-Security-Policy': `default-src 'none'; script-src 'unsafe-inline'; frame-ancestors ${host().base_url}`,
      })
      .end(html);
  });
  await new Promise<void>((resolve) => external.listen(0, '127.0.0.1', resolve));
  const address = external.address();
  if (!address || typeof address === 'string') throw new Error('external address missing');
  origin = `http://127.0.0.1:${address.port}`;
});
test.afterAll(async () => {
  if (external) await new Promise<void>((resolve) => external.close(() => resolve()));
});

test('右栏拖拽区从分割线右侧开始，让出中栏滚动条', async ({ page }) => {
  const title = 'F03 splitter hit area';
  const { session: { session_id } } = await command('create_session', { title });
  const paragraphs = Array.from({ length: 40 }, (_, index) => `滚动测试第 ${index + 1} 段`).join('\n\n');
  const { input_id } = await command('submit_input', {
    session_id, message: `F05_RESOURCES\n${paragraphs}`, variant: 'build', mode: 'normal', attachment_ids: [], quotes: [],
  });
  await expect.poll(async () => {
    const view = await command('get_session_view', { session_id });
    return view.snapshot.value.runs.find((run: { input_id: string }) => run.input_id === input_id)?.status;
  }).toBe('completed');

  await page.setViewportSize({ width: 1440, height: 900 });
  await login(page);
  await page.getByRole('button', { name: title, exact: true }).click();
  const scroll = page.getByLabel('消息列表', { exact: true });
  const handle = page.getByRole('separator', { name: '调整资源栏宽度' });
  await expect(scroll).toBeVisible();
  await expect(handle).toBeVisible();
  const sidebar = page.getByRole('complementary', { name: '资源栏' });
  const width = Number(await handle.getAttribute('aria-valuenow'));
  await expect.poll(async () => (await sidebar.boundingBox())?.width).toBe(width);
  const area = (await scroll.boundingBox())!;
  const edge = area.x + area.width;
  const y = area.y + area.height / 2;
  expect((await handle.boundingBox())!.x).toBeCloseTo(edge, 0);
  expect(await scroll.evaluate((node, point) => node.contains(document.elementFromPoint(point.x, point.y)), { x: edge - 1, y })).toBe(true);

  const bottom = await scroll.evaluate((node) => {
    node.scrollTop = node.scrollHeight;
    return node.scrollTop;
  });
  expect(bottom).toBeGreaterThan(0);
  await page.mouse.move(edge - 1, y);
  await page.mouse.wheel(0, -400);
  await expect.poll(() => scroll.evaluate((node) => node.scrollTop)).toBeLessThan(bottom);

  await page.mouse.move(edge + 2, y);
  await page.mouse.down();
  await expect(page.locator('html')).toHaveAttribute('data-sidebar-resizing', 'right');
  await page.mouse.move(edge - 34, y);
  await page.mouse.up();
  await expect.poll(async () => Number(await handle.getAttribute('aria-valuenow'))).toBeGreaterThan(width);
});

test('无需创建会话即可打开 MCP 应用；草稿替换须确认且不会自动发送', async ({ page }) => {
  const before = await command('list_sessions', { limit: 10000 });
  await login(page);
  await page.getByRole('button', { name: '新建资源标签', exact: true }).click();
  await page.getByRole('menuitem', { name: '应用', exact: true }).click();
  await page.getByRole('button', { name: /MCP fixture app/ }).click();
  await expect(page.getByRole('tab', { name: 'MCP fixture app', exact: true }).locator('[data-icon="puzzle"]')).toBeVisible();
  const panel = page.getByRole('tabpanel').filter({ has: page.getByLabel('应用：MCP fixture app', { exact: true }) });
  const app = panel.frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  await expect(app.getByRole('heading', { name: '标准 MCP 页面' })).toBeVisible();
  await app.getByRole('button', { name: '增加', exact: true }).click();
  await expect(app.locator('#state')).toHaveText('累计 1');
  await page.getByRole('button', { name: '新建资源标签', exact: true }).click();
  await page.getByRole('menuitem', { name: '应用', exact: true }).click();
  const picker = page.getByRole('dialog', { name: '打开应用', exact: true });
  await picker.getByRole('textbox', { name: '外部应用地址' }).fill(origin + '/app');
  await picker.getByRole('button', { name: '连接外部应用', exact: true }).click();
  await page.getByRole('tab', { name: 'MCP fixture app', exact: true }).click();
  await expect(app.locator('#state')).toHaveText('累计 1');

  await expect(app.getByRole('button', { name: '打开应用', exact: true })).toBeDisabled();
  await expect(app.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
  const draft = page.getByRole('textbox', { name: '输入消息', exact: true });
  await draft.fill('用户未发送的草稿');
  await app.getByRole('button', { name: '填写草稿', exact: true }).click();
  const confirmation = page.getByRole('dialog', { name: '替换输入草稿' });
  await expect(confirmation).toBeVisible();
  await confirmation.getByRole('button', { name: '取消', exact: true }).click();
  await expect(draft).toHaveValue('用户未发送的草稿');
  await app.getByRole('button', { name: '填写草稿', exact: true }).click();
  await expect(confirmation).toBeVisible();
  await app.getByRole('button', { name: '导航', exact: true }).evaluate((button: HTMLButtonElement) => button.click());
  await expect(confirmation).toHaveCount(0);
  await expect(draft).toHaveValue('用户未发送的草稿');
  await panel.getByRole('button', { name: '重新连接应用', exact: true }).click();
  await app.getByRole('button', { name: '填写草稿', exact: true }).click();
  await confirmation.getByRole('button', { name: '替换草稿', exact: true }).click();
  await expect(draft).toHaveValue('来自应用的草稿');
  const after = await command('list_sessions', { limit: 10000 });
  expect(after.sessions.map((s: { session_id: string }) => s.session_id)).toEqual(
    before.sessions.map((s: { session_id: string }) => s.session_id),
  );
});

async function installLinks() {
  const archive = resolve(process.env.EZ_ASSISTANT_E2E_TEST_ROOT!, 'link-fixture.zip');
  await promisify(execFile)('python3', [
    '-c',
    `import json,sys,zipfile,copy
from pathlib import Path
base=json.loads(Path(sys.argv[1]).read_text())['base'];base['name']='ui-link-fixture'
card=base['ui']['entries'][0];card['input_schema']['properties']['app_plugin']={'type':'string'}
card['capabilities']=['save-snapshot','open-app','post-message','set-composer-draft']
app=copy.deepcopy(card);app['id']='counter-app';app['title']='Linked counter app';app['kind']='app'
app.pop('snapshot_schema');app.pop('readable_snapshot_schemas');app['capabilities']=['post-message','set-composer-draft']
app['icon']='resources/counter.svg'
base['ui']['entries'].append(app)
with zipfile.ZipFile(sys.argv[3],'w') as z:
 z.writestr('plugin.json',json.dumps(base));z.writestr('resources/counter.html',Path(sys.argv[2]).read_text())
 z.writestr('resources/counter.svg','<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><circle cx="16" cy="16" r="12"/></svg>')`,
    resolve('../../packages/assistant-protocol/fixtures/plugin-ui.json'),
    resolve('../runtime-host/tests/fixtures/mcp_ui_app.html'),
    archive,
  ]);
  const bytes = await readFile(archive);
  const inspected = await fetch(`${host().base_url}/plugins/inspect`, {
    method: 'POST',
    headers: { ...headers(), 'x-ez-plugin-format': 'zip' },
    body: bytes,
  });
  expect(inspected.status).toBe(200);
  const key = crypto.randomUUID();
  await command('begin_plugin_install', { key, packages: [await inspected.json()] });
  const upload = await fetch(`${host().base_url}/plugins/operations/${key}/package`, {
    method: 'POST',
    headers: headers(),
    body: bytes,
  });
  expect(upload.ok).toBeTruthy();
  await expect.poll(async () => (await command('get_plugin_operation', { key })).phase).toBe('succeeded');
}
test('两张卡片复用同一应用，结果回到各自卡片并持久保存；关闭后撤销关联', async ({ page }) => {
  await installLinks();
  const created = await command('create_session', { title: 'F03 linked cards' });
  const session_id = created.session.session_id;
  for (const suffix of ['ONE', 'TWO']) {
    const input = await command('submit_input', {
      session_id,
      message: `F03_APPS_CARD_${suffix}`,
      variant: 'build',
      mode: 'normal',
      attachment_ids: [],
      quotes: [],
    });
    await expect
      .poll(async () => {
        const view = await command('get_session_view', { session_id });
        return view.snapshot.value.runs.find((run: { input_id: string }) => run.input_id === input.input_id)?.status;
      })
      .toBe('completed');
  }
  await login(page);
  await page.getByRole('button', { name: 'F03 linked cards', exact: true }).click();
  const cards = page.getByRole('region', { name: '消息卡片：Counter', exact: true });
  await expect(cards).toHaveCount(2);
  const content = (index: number) =>
    cards.nth(index).frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  await content(0).getByRole('button', { name: '打开应用', exact: true }).click();
  const appPanel = page
    .getByRole('tabpanel')
    .filter({ has: page.getByLabel('应用：Linked counter app', { exact: true }) });
  const app = () => appPanel.frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  await expect(app().getByRole('button', { name: '回传 1', exact: true })).toBeVisible();
  const tab = page.getByRole('tab', { name: 'Linked counter app', exact: true });
  await expect.poll(() => tab.locator('img').evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(32);
  await content(1).getByRole('button', { name: '打开应用', exact: true }).click();
  await expect(app().getByRole('button', { name: '回传 2', exact: true })).toBeVisible();
  await expect(tab).toHaveCount(1);
  await app().getByRole('button', { name: '回传 1', exact: true }).click();
  await expect(content(0).locator('#state')).toHaveText('已保存 11');
  await expect(content(1).locator('#state')).toHaveText('累计 2');
  await app().getByRole('button', { name: '回传 2', exact: true }).click();
  await expect(content(1).locator('#state')).toHaveText('已保存 12');
  await page.getByRole('button', { name: '关闭 Linked counter app', exact: true }).click();
  await expect(tab).toHaveCount(0);
  await page.reload();
  await page.getByRole('button', { name: 'F03 linked cards', exact: true }).click();
  await expect(content(0).locator('#state')).toHaveText('累计 11');
  await expect(content(1).locator('#state')).toHaveText('累计 12');
  const view = await command('get_session_view', { session_id });
  expect(view.snapshot.value.runs).toHaveLength(2);
});

test('外部 HTTP 应用先确认来源，重连重新授权，跨来源跳转撤销桥接', async ({ page }) => {
  await login(page);
  const open = async (path: string) => {
    await page.getByRole('button', { name: '新建资源标签', exact: true }).click();
    await page.getByRole('menuitem', { name: '应用', exact: true }).click();
    const picker = page.getByRole('dialog', { name: '打开应用', exact: true });
    await picker.getByRole('textbox', { name: '外部应用地址' }).fill(origin + path);
    await picker.getByRole('button', { name: '连接外部应用', exact: true }).click();
  };
  await open('/app');
  const panel = page.getByLabel('应用：127.0.0.1', { exact: true });
  await expect(panel.locator('iframe')).toHaveCount(0);
  await expect(panel).toContainText(origin);
  await panel.getByRole('button', { name: '连接', exact: true }).click();
  await expect(panel.getByRole('region', { name: '应用页面', exact: true })).toHaveAttribute(
    'data-app-state',
    'connected',
  );
  await expect(panel.frameLocator('iframe').getByRole('button', { name: '调用应用工具' })).toBeDisabled();
  await panel.getByRole('button', { name: '重新连接应用', exact: true }).click();
  await expect(panel.locator('iframe')).toHaveCount(0);
  await expect(panel.getByRole('button', { name: '连接', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '关闭 127.0.0.1', exact: true }).click();
  await open('/redirect');
  await panel.getByRole('button', { name: '连接', exact: true }).click();
  await expect(panel.getByRole('region', { name: '应用页面', exact: true })).toHaveAttribute('data-app-state', 'error');
  await expect(panel.locator('iframe')).toHaveCount(0);
});

test('草稿页面只调用 app 可见工具，拒绝和导航都取消未执行审批', async ({ page }) => {
  await login(page);
  await page.getByRole('button', { name: '新建资源标签', exact: true }).click();
  await page.getByRole('menuitem', { name: '应用', exact: true }).click();
  await page.getByRole('button', { name: /MCP fixture app/ }).click();
  const panel = page.getByLabel('应用：MCP fixture app', { exact: true });
  const content = panel.frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  const app_tool = content.getByRole('button', { name: '调用应用工具', exact: true });
  const result = content.locator('#tool-result');
  const approval = page.getByRole('dialog', { name: '应用工具授权' });

  await content.getByRole('button', { name: '调用模型工具', exact: true }).click();
  await expect(result).toHaveText('此工具对当前页面不可用。');
  await expect(approval).toHaveCount(0);
  await app_tool.evaluate((button: HTMLButtonElement) => button.click());
  await expect(approval).toContainText('MCP fixture app');
  await expect(approval).toContainText('当前草稿');
  await expect(approval).toContainText('app_only');
  await approval.getByRole('button', { name: '拒绝', exact: true }).click();
  await expect(result).toHaveText('工具调用未获授权。');

  await app_tool.evaluate((button: HTMLButtonElement) => button.click());
  await expect(approval).toBeVisible();
  await content.getByRole('button', { name: '导航', exact: true }).evaluate((button: HTMLButtonElement) => button.click());
  await expect(approval).toHaveCount(0);
  await panel.getByRole('button', { name: '重新连接应用', exact: true }).click();
  await app_tool.click();
  await expect(result).toHaveText('App calls 1');
  await expect(approval).toHaveCount(0);
  await expect(result).toHaveAttribute('data-private', 'tool-private');
});

test('页面调用按真实会话权限复核，拒绝迟到及重复请求', async () => {
  const { session } = await command('create_session', { title: 'F03 app tool permissions' });
  const target = {
    type: 'mcp', target: { source: { type: 'user' }, server_key: 'ui_fixture' },
    resource_uri: 'ui://fixture/card', entry_id: 'app',
  };
  const { tool_instance_id } = await command('read_ui_app', {
    target, arguments: {}, mount: { type: 'session', session_id: session.session_id },
  });
  expect(tool_instance_id).toBeTruthy();
  const send = (action: object) => command('ui_tool', { instance_id: tool_instance_id, action });
  const start = (request_id: string, user_gesture?: boolean) =>
    send({ type: 'start', request_id, name: 'app_only', arguments: { case: 'permission' }, user_gesture });
  const poll = (request_id: string) => send({ type: 'poll', request_id });
  const scope = { type: 'session', payload: { session_id: session.session_id } };
  async function setRule(effect: 'allow' | 'ask' | 'deny') {
    const current = (await command('get_permission_document', { scope })).document;
    await command('replace_permission_document', {
      scope, expected_revision: current.revision, document: {
        schema_version: 2,
        rules: [{
          id: 'fixture-app-tool', effect, variants: ['build', 'plan'],
          matcher: { type: 'mcp', payload: {
            server: { type: 'exact', payload: { server_key: 'ui_fixture' } },
            tool: { type: 'exact', payload: { tool_name: 'app_only' } },
          } },
        }],
      },
    });
  }
  await expect(start('waiting')).resolves.toMatchObject({ type: 'pending' });
  let approval: UiToolApproval | undefined;
  await expect.poll(async () => {
    const state = await poll('waiting');
    approval = state.approval;
    return state.type;
  }).toBe('approval');
  if (!approval) throw new Error('Expected a page approval');
  expect(approval.scope).toEqual({ type: 'session', session_id: session.session_id });
  await setRule('deny');
  await send({ type: 'decide', request_id: 'waiting', approval_id: approval.approval_id, decision: 'allow_once' });
  await expect.poll(() => poll('waiting')).toMatchObject({
    type: 'failed', reason: 'ui_tool_permission_denied', remote_may_have_executed: false,
  });
  await start('denied-gesture', true);
  await expect.poll(() => poll('denied-gesture')).toMatchObject({
    type: 'failed', reason: 'ui_tool_permission_denied', remote_may_have_executed: false,
  });

  await setRule('allow');
  await send({ type: 'cancel', request_id: 'cancelled-before-start' });
  await expect(start('cancelled-before-start')).resolves.toMatchObject({ type: 'failed', reason: 'ui_tool_duplicate_request' });
  await start('permitted');
  await expect(start('permitted')).resolves.toMatchObject({ type: 'failed', reason: 'ui_tool_duplicate_request' });
  await expect.poll(() => poll('permitted')).toMatchObject({
    type: 'complete', result: { content: [{ type: 'text', text: 'App calls 1' }] },
  });
  const view = await command('get_session_view', { session_id: session.session_id });
  expect(view.snapshot.value.runs).toEqual([]);

  await setRule('ask');
  await start('retired', true);
  await expect.poll(async () => (await poll('retired')).type).toBe('approval');
  const configuration = (await command('get_mcp_configuration', {})).snapshot;
  await command('refresh_mcp_server', {
    target: target.target, expected_revision: configuration.revision, expected_definition_digest: '',
  });
  await expect.poll(() => poll('retired')).toMatchObject({
    type: 'failed', reason: 'ui_tool_source_retired', remote_may_have_executed: false,
  });
  await send({ type: 'release' });
});
