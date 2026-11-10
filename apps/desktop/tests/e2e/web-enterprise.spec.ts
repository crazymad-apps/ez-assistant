import { test, expect, type Page } from '@playwright/test';
import { createServer } from 'node:http';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { compatibilityHeaders } from '@ez-assistant/protocol';

// 可控 Center 只维护测试内存凭据；正式 Host 使用全新临时 Home，无共享中心或用户数据。
const tokens = new Map<string, number>();
const organization_id = '01234567-89ab-4cde-8f01-23456789abce';
const organization = { kind: 'organization', organization_id, name: '测试组织', enabled: true, member_role: 'member' };
let serial = 0,
  logout_count = 0,
  home = '',
  address = '',
  management = '';
let host: ChildProcess;
let managed_model: object | null = null;
let model_refresh_fails = false;
let additional_organization_enabled = false;
const additional_organization = { ...organization, organization_id: '01234567-89ab-4cde-8f01-23456789abcf', name: '空模型组织' };
const token_scopes = new Map<string, typeof organization>();
const model_calls: { path: string; model: string; organization_credential: boolean }[] = [];
let release_model: (() => void) | undefined;
let hold_model: Promise<void> | undefined;
let hold_prompt: string | undefined;
const identity = (id: number) => ({
  center_id: '01234567-89ab-4cde-8f01-23456789abcd',
  legacy_organization_id: organization_id,
  expires_at: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
  scope: organization,
  user: {
    id,
    username: id === 1 ? 'alice' : 'bob',
    display_name: id === 1 ? 'Alice' : 'Bob',
    role: 'user',
    enabled: true,
    is_super_admin: false,
  },
});
const center = createServer(async (request, response) => {
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(Buffer.from(chunk));
  const input = Buffer.concat(chunks).toString();
  const body = input ? JSON.parse(input) : {};
  const token = request.headers.authorization?.slice(7) ?? '';
  const id = tokens.get(token);
  response.setHeader('Content-Type', 'application/json');
  if (request.url === '/api/info')
    return response.end(
      JSON.stringify({ center_id: identity(1).center_id, protocol_version: 6, min_protocol_version: 6, capabilities: ['managed_models', 'llm_proxy'] }),
    );
  if (request.url === '/api/auth/login' && body.password === 'fixture-password') {
    const id = body.username === 'alice' ? 1 : 2;
    const suffix = (++serial).toString(16).padStart(64, '0');
    const unselected = `pending.${suffix}.fixture`;
    tokens.set(unselected, id);
    return response.end(JSON.stringify({ ...identity(id), token: unselected, scope: { kind: 'unselected' } }));
  }
  if (request.url === '/api/auth/scopes?entry=client' && id) {
    return response.end(JSON.stringify(additional_organization_enabled ? [organization, additional_organization] : [organization]));
  }
  const scopes = additional_organization_enabled ? [organization, additional_organization] : [organization];
  const requested_scope = scopes.find(scope => scope.organization_id === body.organization_id);
  if (request.url === '/api/auth/scope' && id && body.entry === 'client' && requested_scope) {
    tokens.delete(token);
    token_scopes.delete(token);
    const suffix = (++serial).toString(16).padStart(64, '0');
    const selected = `selected.${suffix}.fixture`;
    tokens.set(selected, id);
    token_scopes.set(selected, requested_scope);
    return response.end(JSON.stringify({ ...identity(id), scope: requested_scope, token: selected, llm_key: `cl_${suffix}` }));
  }
  const scope = token_scopes.get(token) ?? organization;
  if (request.url === '/api/runtime/model-configuration' && id) {
    if (model_refresh_fails) {
      response.statusCode = 503;
      return response.end('{}');
    }
    return response.end(
      JSON.stringify(
        (scope.organization_id === organization_id ? managed_model : null) ??
          { center_id: identity(id).center_id, organization_id: scope.organization_id, allow_member_selection: false, default_model: null, providers: [] },
      ),
    );
  }
  if (request.url === '/api/auth/me' && id) return response.end(JSON.stringify({ ...identity(id), scope }));
  if (request.url === '/api/auth/password' && id) {
    response.statusCode = 204;
    return response.end();
  }
  if (request.url === '/api/auth/logout' && id) {
    tokens.delete(token);
    token_scopes.delete(token);
    logout_count++;
    response.statusCode = 204;
    return response.end();
  }
  const personal = request.headers.authorization === 'Bearer f02-personal-fixture-key';
  if (request.url === '/personal/v1/models' && personal) {
    return response.end(JSON.stringify({ object: 'list', data: [{ id: 'gpt-4.1', object: 'model' }] }));
  }
  const organization_request = request.url === `/api/llm/providers/${identity(1).center_id}/v1/chat/completions`
    && [...tokens.keys()].some(key => key.startsWith('selected.') && request.headers.authorization === `Bearer cl_${key.split('.')[1]}`);
  if (request.url && (organization_request || (request.url === '/personal/v1/chat/completions' && personal))) {
    model_calls.push({ path: request.url, model: body.model, organization_credential: organization_request });
    if (hold_prompt && input.includes(hold_prompt)) await hold_model;
    response.writeHead(200, { 'Content-Type': 'text/event-stream' });
    const id = `f02-response-${model_calls.length}`;
    const text = `${organization_request ? '企业' : '个人'}回复 ${body.model} ${model_calls.length}`;
    for (const [delta, finish_reason] of [[{ role: 'assistant', content: text }, null], [{}, 'stop']]) {
      response.write(`data: ${JSON.stringify({ id, model: body.model, choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
    }
    return response.end('data: [DONE]\n\n');
  }
  response.statusCode = 401;
  response.end(JSON.stringify({ error: { code: 'TOKEN_INVALID' } }));
});

test.beforeAll(async () => {
  await new Promise<void>((done) => center.listen(0, '127.0.0.1', done));
  const center_port = (center.address() as { port: number }).port;
  const port_probe = createServer();
  await new Promise<void>((done) => port_probe.listen(0, '127.0.0.1', done));
  const port = (port_probe.address() as { port: number }).port;
  await new Promise<void>((done) => port_probe.close(() => done()));
  const test_root = resolve('../../.runtime-test/c04-host-web');
  await mkdir(test_root, { recursive: true });
  home = await mkdtemp(join(test_root, 'web-'));
  await mkdir(join(home, 'os-home'));
  await writeFile(
    join(home, 'host.toml'),
    `version = "0.27.0"\nmode = "enterprise"\n[enterprise]\ncenter_url = "http://127.0.0.1:${center_port}"\n[host_access]\nport = ${port}\nremote_enabled = false\n`,
  );
  // 集成验收可指定 Release Host，避免重建或替换正在用于人工测试的 debug 二进制。
  const executable = process.env.EZ_ASSISTANT_E2E_HOST_EXECUTABLE ?? resolve('../../target/debug/ez-assistant-runtime');
  host = spawn(executable, ['serve', '--runtime-home', home], {
    env: { ...process.env, HOME: join(home, 'os-home') },
    stdio: 'ignore',
  });
  await expect
    .poll(
      async () => {
        if (host.exitCode !== null) throw new Error(`隔离 Host 启动失败：退出码 ${host.exitCode}`);
        try {
          const info = JSON.parse(await readFile(join(home, 'run/runtime.json'), 'utf8'));
          address = info.address;
          management = info.access_token;
          return !!address;
        } catch {
          return false;
        }
      },
      { timeout: 15_000 },
    )
    .toBe(true);
});
test.afterAll(async () => {
  if (host?.exitCode === null) {
    host.kill('SIGTERM');
    await new Promise<void>((done) => host.once('exit', () => done()));
  }
  center.closeAllConnections();
  await new Promise<void>((done) => center.close(() => done()));
  if (home && process.env.C03_E2E_RETAIN_HOME === '1') {
    process.stdout.write(`retained enterprise E2E Runtime Home: ${home}\n`);
  } else if (home) await rm(home, { recursive: true, force: true });
});
function accountName(page: Page, name: string) {
  return page.getByRole('group', { name: '当前账号' }).getByText(name, { exact: true });
}

async function login(page: Page, username: string) {
  await page.goto(address);
  await page.getByLabel('企业账号', { exact: true }).fill(username);
  await page.getByLabel('账号密码', { exact: true }).fill('fixture-password');
  await page.getByRole('button', { name: '进入项目' }).click();
  await page.getByRole('button', { name: '进入组织', exact: true }).click();
  await expect(accountName(page, username === 'alice' ? 'Alice' : 'Bob')).toBeVisible();
}
async function nativeLogin(username: string): Promise<{ token: string }> {
  const post = (body: object) => fetch(`${address}/auth/login`, {
    method: 'POST',
    headers: { ...compatibilityHeaders(), 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  const login = await post({ method: 'enterprise', username, password: 'fixture-password', native: true });
  expect(login.status).toBe(200);
  const pending = await login.json();
  expect(pending.token).toBeNull();
  const selected = await post({ method: 'enterprise_select', selection_token: pending.organization_selection.selection_token, organization_id });
  expect(selected.status).toBe(200);
  return selected.json();
}
async function logout(page: Page, user: string, cancel = false) {
  await expect(accountName(page, user)).toBeVisible();
  await page.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
  await page.getByRole('menuitem', { name: '退出登录' }).click();
  await expect(page.getByRole('dialog', { name: '确认退出登录' })).toContainText('所有任务');
  await page.getByRole('button', { name: cancel ? '取消' : '中断所有任务并退出', exact: true }).click();
}

test('登录表单在常用矮窗口完整显示，Desktop 本机和远端字段保持间距', async ({ page }, info) => {
  await page.setViewportSize({ width: 1000, height: 600 });
  await page.goto(address);
  await expect(page.getByLabel('企业账号', { exact: true })).toBeVisible();
  const expectCompactEntry = async () => {
    await expect(page.getByRole('button', { name: '管理本机 Host' })).toHaveCount(0);
    await expect(page.getByRole('heading', { name: '连接你的项目' })).toHaveCount(0);
    expect(await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight)).toBe(true);
  };
  await expectCompactEntry();
  // 仅替换 Desktop 原生桥，页面和企业 Host 发现仍使用产品实现。
  const capabilities = await (await fetch(`${address}/capabilities`)).json();
  await page.addInitScript(
    ({ address, capabilities }) => {
      let callback = 0;
      Object.defineProperty(globalThis, 'isTauri', { value: true });
      Object.defineProperty(globalThis, '__TAURI_EVENT_PLUGIN_INTERNALS__', { value: { unregisterListener() {} } });
      Object.defineProperty(globalThis, '__TAURI_INTERNALS__', {
        value: {
          transformCallback: () => ++callback,
          async invoke(command: string) {
            if (command === 'bootstrap_runtime')
              return {
                base_url: address,
                instance_id: 'layout-fixture',
                access_token: '',
                capabilities,
                started_runtime: false,
              };
            if (command === 'desktop_platform') return 'macos';
            if (command === 'plugin:event|listen') return ++callback;
            return null;
          },
        },
      });
    },
    { address, capabilities },
  );
  await page.reload();
  await expect(page.getByText('本机 Runtime 可连接', { exact: true })).toBeVisible();
  await expectCompactEntry();
  const account = page.getByText('企业账号', { exact: true });
  const local = await page.getByText('这台电脑', { exact: true }).locator('../..').boundingBox();
  const local_account = await account.boundingBox();
  expect(local_account!.y - local!.y - local!.height).toBeGreaterThanOrEqual(17);
  await page.screenshot({ path: info.outputPath('desktop-login-local.png') });
  await page.getByRole('button', { name: '其他 Runtime', exact: true }).click();
  await expectCompactEntry();
  const origin = await page.getByLabel('Host 地址', { exact: true }).boundingBox();
  const remote_account = await account.boundingBox();
  expect(remote_account!.y - origin!.y - origin!.height).toBeGreaterThanOrEqual(17);
  await page.screenshot({ path: info.outputPath('desktop-login-remote.png') });
});

test('企业登录、阻塞重试、改密、退出取消与多浏览器隔离', async ({ browser }, info) => {
  const a = await browser.newContext({ reducedMotion: 'reduce' }),
    b = await browser.newContext({ reducedMotion: 'reduce' });
  const page = await a.newPage(),
    bob = await b.newPage();
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  let readiness = 0;
  await page.route('**/runtime/ensure-ready', async (route) => {
    readiness++;
    if (readiness === 1)
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'storage_unavailable', message: '隔离测试初始化失败' } }),
      });
    else await route.continue();
  });
  await page.goto(address);
  await page.getByLabel('企业账号', { exact: true }).fill('alice');
  await page.getByLabel('账号密码', { exact: true }).fill('fixture-password');
  await page.getByRole('button', { name: '进入项目' }).click();
  await page.getByRole('button', { name: '进入组织', exact: true }).click();
  await expect(page.getByRole('button', { name: '重试初始化' })).toBeVisible();
  expect(readiness).toBe(1);
  await page.getByRole('button', { name: '重试初始化' }).click();
  await expect(accountName(page, 'Alice')).toBeVisible();
  await login(bob, 'bob');
  await page.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
  await expect(page.getByRole('menu', { name: '账号菜单' }).getByRole('menuitem')).toHaveText([
    '测试组织', '设置', '反馈中心', '修改密码', '退出登录',
  ]);
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.getByRole('menuitem', { name: '修改密码', exact: true }).click();
  await page.getByLabel('当前密码', { exact: true }).fill('fixture-password');
  await page.getByLabel('新密码', { exact: true }).fill('changed-password');
  const password_dialog = page.getByRole('dialog', { name: '修改密码', exact: true });
  await expect(password_dialog.locator('footer').getByRole('button')).toHaveText(['关闭', '修改密码']);
  await expect(password_dialog.locator('footer').getByRole('button', { name: '修改密码', exact: true })).toBeEnabled();
  await page.getByLabel('新密码', { exact: true }).press('Enter');
  await expect(page.getByText('密码已修改，已有登录和运行中任务保持有效。', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '关闭', exact: true }).click();
  const before = logout_count;
  await logout(page, 'Alice', true);
  expect(logout_count).toBe(before);
  await expect(accountName(page, 'Alice')).toBeVisible();
  await page.reload();
  await expect(accountName(page, 'Alice')).toBeVisible();
  await logout(page, 'Alice');
  await expect(page.getByLabel('企业账号', { exact: true })).toBeVisible();
  await expect(accountName(bob, 'Bob')).toBeVisible();
  await bob.screenshot({ path: info.outputPath('enterprise-bob.png') });
  expect(errors).toEqual([]);
  await a.close();
  await b.close();
});

test('同源标签页与快捷 Token 换号后重新核验且清除旧投影', async ({ browser }) => {
  const context = await browser.newContext({ reducedMotion: 'reduce' });
  const first = await context.newPage();
  await login(first, 'alice');
  const second = await context.newPage();
  await second.goto(address);
  await expect(accountName(second, 'Alice')).toBeVisible();
  const { token } = await nativeLogin('bob');
  await second.goto(`${address}/#token=${token}`);
  await expect(accountName(second, 'Bob')).toBeVisible();
  await first.bringToFront();
  await expect(accountName(first, 'Bob')).toBeVisible();
  expect(new URL(second.url()).hash).toBe('');
  expect(await first.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toMatch(
    /ct_|cl_|session:|login_context/,
  );
  await context.close();
});

test('管理配置保存后当前模式保持冻结并要求重启', async () => {
  // 本用例自行建立中心绑定，不依赖前面浏览器用例的执行顺序。
  await nativeLogin('alice');
  const command = async (payload: object) => {
    const response = await fetch(`${address}/commands`, {
      method: 'POST',
      headers: { ...compatibilityHeaders(), Authorization: `Bearer ${management}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ request_id: 'fixture-config', command: { scope: 'host_access', payload } }),
    });
    expect(response.status).toBe(200);
    return (await response.json()).result.payload;
  };
  const status = await command({ type: 'get_status' });
  expect(status.mode).toBe('enterprise');
  expect(status.center_id).toBeTruthy();
  const saved = await command({
    type: 'configure',
    payload: { expected_revision: status.revision, configuration: status.configuration, mode: 'personal' },
  });
  expect(saved.mode).toBe('personal');
  expect(saved.restart_required).toBe(true);
  expect((await (await fetch(`${address}/capabilities`)).json()).mode).toBe('enterprise');
  // 共用隔离 Host；恢复持久模式，避免后续登录被前一个场景的配置拒绝。
  await command({
    type: 'configure',
    payload: {
      expected_revision: saved.revision,
      configuration: status.configuration,
      mode: status.mode,
      center_url: status.center_url,
    },
  });
});

test('中心模型只读展示、刷新切换与失败保留', async ({ page }, info) => {
  const port = (center.address() as { port: number }).port;
  const parameters = {
    context_window_tokens: { state: 'known', value: 8192 },
    max_input_tokens: { state: 'unknown' },
    max_output_tokens: { state: 'known', value: 1024 },
    reasoning_max_input_tokens: { state: 'unknown' },
    reasoning_max_output_tokens: { state: 'unknown' },
    streaming: 'supported',
    image_input: 'unsupported',
    tool_calls: 'unsupported',
    reasoning: 'unsupported',
    reasoning_mode: 'unsupported',
    tool_choice: { auto: 'unknown', none: 'unknown', required: 'unknown', named: 'unknown' },
    tool_image_projection: 'unsupported',
    reasoning_efforts: null,
    default_reasoning_effort: null,
  };
  const provider_instance_id = identity(1).center_id;
  const configuration = (model_id: string, enabled = true) => ({
    center_id: identity(1).center_id,
    organization_id,
    allow_member_selection: false,
    default_model: { provider_instance_id, model_id },
    providers: [{
      provider_instance_id,
      endpoint: `http://127.0.0.1:${port}/api/llm/providers/${provider_instance_id}/v1`,
      provider_type: 'openai',
      provider_display_name: '企业测试服务商',
      protocol: 'open_ai_responses',
      server_history_enabled: enabled,
      server_history_support: 'supported',
      models: [{ state: 'ready', model_id, parameters, server_history_binding: enabled ? 'a'.repeat(64) : null }],
    }],
  });
  managed_model = configuration('center-model-a');
  await login(page, 'alice');
  await page.getByRole('button', { name: '模型设置', exact: true }).click();
  await page.getByRole('menuitem', { name: '企业测试服务商', exact: true }).click();
  await expect(page.getByRole('menuitemradio', { name: 'center-model-a', exact: true })).toBeEnabled();
  await page.screenshot({ path: info.outputPath('managed-model-menu.png'), animations: 'disabled' });
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
  await page.getByRole('menuitem', { name: '设置', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '设置', exact: true });
  await dialog.getByRole('button', { name: '模型', exact: true }).click();
  const default_model = dialog.getByRole('group', { name: '组织默认模型' });
  await expect(default_model).toContainText('center-model-a');
  await expect(dialog.getByRole('button', { name: '添加服务商' })).toBeEnabled();
  await dialog.getByRole('button', { name: /管理服务商 企业测试服务商/ }).click();
  await expect(dialog.getByText('由企业管理员管理；已启用', { exact: true })).toBeVisible();
  await expect(dialog.getByRole('button', { name: '编辑', exact: true })).toHaveCount(0);
  await dialog.getByRole('button', { name: '返回', exact: true }).click();
  managed_model = configuration('center-model-b', false);
  await dialog.getByRole('button', { name: '刷新配置' }).click();
  await expect(default_model).toContainText('center-model-b');
  await dialog.getByRole('button', { name: /管理服务商 企业测试服务商/ }).click();
  await expect(dialog.getByText('由企业管理员管理；已关闭', { exact: true })).toBeVisible();
  await expect(dialog.getByRole('button', { name: '编辑', exact: true })).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: '添加模型' })).toHaveCount(0);
  await dialog.getByRole('button', { name: '配置模型 center-model-b', exact: true }).click();
  await expect(dialog.getByLabel('上下文窗口（Token）', { exact: true })).toBeDisabled();
  await expect(dialog.getByLabel('上下文窗口（Token）', { exact: true })).toHaveValue('8192');
  await page.screenshot({ path: info.outputPath('managed-model-ready.png'), animations: 'disabled' });
  await dialog.getByRole('button', { name: '返回', exact: true }).click();
  await dialog.getByRole('button', { name: '返回', exact: true }).click();
  model_refresh_fails = true;
  await dialog.getByRole('button', { name: '刷新配置' }).click();
  await expect(default_model).toContainText('center-model-b');
  await expect(dialog.getByText(/^刷新失败：/)).toBeVisible();
  model_refresh_fails = false;
  managed_model = null;
  await dialog.getByRole('button', { name: '刷新配置' }).click();
  await expect(dialog.getByText('管理员尚未配置组织默认模型。', { exact: true })).toBeVisible();
  await expect(dialog.getByText('center-model-b', { exact: true })).toHaveCount(0);
  await page.screenshot({ path: info.outputPath('managed-model-unavailable.png'), animations: 'disabled' });
});

test('企业与个人模型同名时按来源执行，组织切换后恢复独立配置', async ({ page }, info) => {
  const port = (center.address() as { port: number }).port;
  const provider_instance_id = identity(1).center_id;
  const default_id = 'gpt-4.1';
  const alternative_id = 'f02-organization-alt';
  const provider_name = '同名服务商';
  const parameters = {
    context_window_tokens: { state: 'known', value: 16384 },
    max_input_tokens: { state: 'unknown' },
    max_output_tokens: { state: 'known', value: 1024 },
    reasoning_max_input_tokens: { state: 'unknown' },
    reasoning_max_output_tokens: { state: 'unknown' },
    streaming: 'supported',
    image_input: 'supported',
    tool_calls: 'supported',
    reasoning: 'unsupported',
    reasoning_mode: 'unsupported',
    tool_choice: { auto: 'supported', none: 'supported', required: 'supported', named: 'supported' },
    tool_image_projection: 'unsupported',
    reasoning_efforts: null,
    default_reasoning_effort: null,
  };
  const configuration = (default_model: string, visible: string[]) => ({
    center_id: identity(1).center_id, organization_id,
    allow_member_selection: visible.length > 1,
    default_model: { provider_instance_id, model_id: default_model },
    providers: [{
      provider_instance_id,
      endpoint: `http://127.0.0.1:${port}/api/llm/providers/${provider_instance_id}/v1`,
      provider_type: 'openai', provider_display_name: provider_name, protocol: 'open_ai_chat_completions',
      server_history_enabled: false, server_history_support: 'unsupported',
      models: visible.map(model_id => ({ state: 'ready', model_id, parameters, server_history_binding: null })),
    }],
  });
  const model_trigger = page.getByRole('button', { name: '模型设置', exact: true });
  const model_menu = page.getByRole('menu', { name: '模型设置', exact: true });
  const follow = page.getByRole('menuitemradio', { name: '跟随组织默认', exact: true });
  const dialog = page.getByRole('dialog', { name: '设置', exact: true });
  const button = (name: string) => dialog.getByRole('button', { name, exact: true });
  const provider_options = () => page.getByRole('menuitem', { name: provider_name, exact: true });
  async function openSettings() {
    await page.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
    await page.getByRole('menuitem', { name: '设置', exact: true }).click();
    await button('模型').click();
  }
  async function closeSettings() { await button('关闭设置').click(); }
  async function switchOrganization(from: string, to: string) {
    await page.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
    await page.getByRole('menuitem', { name: `切换组织，当前组织：${from}` }).click();
    await page.getByRole('menuitem', { name: to, exact: true }).click();
    await page.getByRole('dialog', { name: `切换到「${to}」` }).getByRole('button', { name: '切换组织', exact: true }).click();
    await expect(page.getByRole('dialog', { name: `切换到「${to}」` })).toHaveCount(0);
  }
  async function pick(index: number, model: string) {
    await model_trigger.click();
    await provider_options().nth(index).click();
    await page.getByRole('menuitemradio', { name: model, exact: true }).click();
    await expect(model_trigger).toHaveAttribute('aria-expanded', 'false');
  }
  async function send(text: string) {
    await page.getByRole('textbox', { name: '输入消息', exact: true }).fill(text);
    await page.getByRole('button', { name: '发送消息', exact: true }).click();
  }
  managed_model = configuration(default_id, [default_id, alternative_id]);
  try {
    await page.setViewportSize({ width: 1200, height: 850 });
    await login(page, 'alice');
    await expect(model_trigger).toContainText('跟随组织默认');
    await model_trigger.click();
    await expect(follow).toHaveAttribute('aria-checked', 'true');
    await expect(model_menu.getByRole('separator')).toHaveCount(0);
    await expect(provider_options()).toHaveCount(1);
    await page.keyboard.press('Escape');
    await pick(0, alternative_id);
    await expect(model_trigger).toContainText(`企业模型 · ${provider_name} / ${alternative_id}`);
    await send('F02 通过企业代理执行');
    await expect(page.getByText(new RegExp(`^企业回复 ${alternative_id} `)).first()).toBeVisible();
    expect(model_calls.some(call => call.organization_credential && call.model === alternative_id)).toBe(true);

    await openSettings();
    await button('添加服务商').click();
    await dialog.getByLabel('服务商名称', { exact: true }).fill(provider_name);
    await dialog.getByLabel('服务地址', { exact: true }).fill(`http://127.0.0.1:${port}/personal/v1`);
    await dialog.getByLabel('API Key', { exact: true }).fill('f02-personal-fixture-key');
    await button('接口选项').click();
    await button('接口协议').click();
    await page.getByRole('option', { name: 'Chat Completions', exact: true }).click();
    await button('保存服务商').click();
    await expect(dialog.getByText('已设置', { exact: true })).toBeVisible();
    await button('刷新在线模型').click();
    await button(`配置模型 ${default_id}`).click();
    await dialog.getByLabel('运行输出上限（Token）', { exact: true }).fill('1024');
    await button('保存固定配置').click();
    await expect(page.getByText('已固定，下次执行生效。', { exact: true })).toBeVisible();
    await button('返回').click();
    await button('编辑').click();
    await expect(dialog.getByLabel('API Key', { exact: true })).toHaveValue('');
    expect(await page.evaluate(() => [localStorage, sessionStorage].some(storage =>
      Object.keys(storage).some(key => storage.getItem(key)?.includes('f02-personal-fixture-key')),
    ))).toBe(false);
    await button('返回').click();
    await button('返回').click();

    await button('默认识图模型').click();
    await expect(follow).toHaveCount(0);
    await provider_options().first().click();
    await page.getByRole('menuitemradio', { name: default_id, exact: true }).click();
    await expect(button('默认识图模型')).toContainText(`企业模型 · ${provider_name}`);
    await button('默认识图模型').click();
    await provider_options().last().click();
    await page.getByRole('menuitemradio', { name: default_id, exact: true }).click();
    await expect(button('默认识图模型')).toContainText(`个人模型 · ${provider_name}`);
    await button('清除默认识图模型').click();
    await expect(button('默认识图模型')).toContainText('未配置');
    await closeSettings();
    await expect(model_trigger).toContainText(alternative_id);

    await model_trigger.click();
    await expect(follow).toHaveAttribute('aria-checked', 'false');
    await expect(model_menu.getByText('企业模型', { exact: true })).toBeVisible();
    await expect(model_menu.getByText('个人模型', { exact: true })).toBeVisible();
    // 推理强度另有分隔线，来源分隔线应紧靠个人分组标题。
    await expect.poll(() => model_menu.getByRole('separator').evaluateAll(
      lines => lines.filter(line => line.nextElementSibling?.textContent === '个人模型').length,
    )).toBe(1);
    await expect(provider_options().first()).toHaveAccessibleDescription('企业模型');
    await expect(provider_options().last()).toHaveAccessibleDescription('个人模型');
    await expect(follow).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(provider_options().first()).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(provider_options().last()).toBeFocused();
    await page.screenshot({ path: info.outputPath('dual-source-model-picker.png'), animations: 'disabled' });
    await page.keyboard.press('ArrowRight');
    await page.getByRole('menuitemradio', { name: default_id, exact: true }).click();
    await expect(model_trigger).toContainText(`个人模型 · ${provider_name} / ${default_id}`);
    hold_prompt = 'F02_QUEUE_HOLD';
    hold_model = new Promise<void>(resolve => { release_model = resolve; });
    await send(hold_prompt);
    await expect.poll(() => model_calls.some(call => call.path === '/personal/v1/chat/completions' && call.model === default_id && !call.organization_credential)).toBe(true);
    await expect(model_trigger).toHaveAttribute('aria-disabled', 'true');
    await send('F02 已排队的输入');
    await expect(page.getByRole('region', { name: '输入队列' }).getByText('F02 已排队的输入')).toBeVisible();
    await expect(model_trigger).toHaveAttribute('aria-disabled', 'true');
    hold_prompt = undefined;
    release_model?.();
    await expect(page.getByText(/^个人回复 gpt-4\.1 /).first()).toBeVisible();
    await expect(model_trigger).toBeEnabled();

    await pick(0, alternative_id);
    managed_model = configuration(default_id, [default_id]);
    await openSettings();
    await button('刷新配置').click();
    await expect(button('刷新配置')).toBeEnabled();
    await closeSettings();
    await expect(model_trigger).toContainText(`${alternative_id}（不可用）`);
    await model_trigger.click();
    await expect(follow).toHaveAttribute('aria-checked', 'false');
    await provider_options().first().click();
    await page.getByRole('menuitemradio', { name: default_id, exact: true }).click();
    await expect(model_trigger).toContainText(`企业模型 · ${provider_name} / ${default_id}`);
    await model_trigger.click();
    await expect(follow).toHaveAttribute('aria-checked', 'false');
    await follow.click();
    await expect(model_trigger).toContainText('跟随组织默认');
    await send('F02 跟随组织默认');
    await expect(page.getByText(/^企业回复 gpt-4\.1 /).first()).toBeVisible();

    managed_model = null;
    await openSettings();
    await button('刷新配置').click();
    await expect(dialog.getByText('管理员尚未配置组织默认模型。', { exact: true })).toBeVisible();
    await closeSettings();
    await model_trigger.click();
    await expect(model_menu.getByRole('separator')).toHaveCount(0);
    await expect(provider_options()).toHaveCount(1);
    await page.keyboard.press('Escape');

    additional_organization_enabled = true;
    await switchOrganization('测试组织', '空模型组织');
    await model_trigger.click();
    await expect(follow).toHaveAttribute('aria-checked', 'true');
    await expect(model_menu.getByText('暂无服务商', { exact: true })).toBeVisible();
    await expect(provider_options()).toHaveCount(0);
    await expect(model_menu.getByRole('separator')).toHaveCount(0);
    await page.keyboard.press('Escape');
    await switchOrganization('空模型组织', '测试组织');
    await openSettings();
    await dialog.getByRole('button', { name: /^管理服务商 同名服务商 http/ }).click();
    await expect(button(`配置模型 ${default_id}`)).toBeVisible();
    await button('删除服务商').click();
    const delete_dialog = page.getByRole('dialog', { name: `删除“${provider_name}”？` });
    await delete_dialog.getByRole('button', { name: '删除服务商', exact: true }).click();
    await expect(dialog.getByText('添加服务商后即可配置个人模型。', { exact: true })).toBeVisible();
  } finally {
    hold_prompt = undefined;
    release_model?.();
    additional_organization_enabled = false;
    managed_model = null;
  }
});

test('仅有页面工具审批时，组织切换等待收尾并撤销原页面句柄', async () => {
  const { token } = await nativeLogin('alice');
  additional_organization_enabled = true;
  const post = (path: string, credential: string, body?: object) => fetch(address + path, {
    method: 'POST',
    headers: { ...compatibilityHeaders(), Authorization: 'Bearer ' + credential, 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  const send = (credential: string, type: string, payload?: object) => post('/commands', credential, {
    request_id: crypto.randomUUID(), command: { scope: 'runtime', payload: { type, payload } },
  });
  const command = async (type: string, payload: object) => {
    const result = await send(token, type, payload);
    expect(result.status, type).toBe(200);
    return (await result.json()).result.payload.payload;
  };
  const change = async (body: object) => {
    const result = await post('/auth/organization-switch', token, body);
    expect(result.status).toBe(200);
    return result.json();
  };
  try {
    expect((await post('/runtime/ensure-ready', token)).status).toBe(204);
    const target = { source: { type: 'user' }, server_key: 'ui_fixture' };
    const before = (await command('get_mcp_configuration', {})).snapshot;
    const { snapshot } = await command('mutate_mcp_configuration', {
      expected_revision: before.revision,
      mutation: { type: 'import', payload: {
        document: JSON.stringify({ mcpServers: {
          ui_fixture: { command: process.platform === 'win32' ? 'python' : 'python3',
            args: ['-u', resolve('../runtime-host/tests/fixtures/mcp_ui_server.py')] },
        } }),
      } },
    });
    await command('refresh_mcp_server', {
      target, expected_revision: snapshot.revision, expected_definition_digest: '',
    });
    const { tool_instance_id: instance_id } = await command('read_ui_app', {
      target: { type: 'mcp', target, resource_uri: 'ui://fixture/card', entry_id: 'app' },
      arguments: {}, mount: { type: 'draft' },
    });
    expect(instance_id).toBeTruthy();
    const request_id = 'fixture-app-switch';
    await command('ui_tool', { instance_id, action: { type: 'start', request_id, name: 'app_only', arguments: {} } });
    await expect.poll(async () =>
      (await command('ui_tool', { instance_id, action: { type: 'poll', request_id } })).type,
    ).toBe('approval');
    const preview = await change({ action: 'preview', organization_id: additional_organization.organization_id });
    expect(preview).toMatchObject({ state: 'confirm', tasks: 1, approvals: 1, queued: 0 });
    await change({ action: 'confirm', operation_id: preview.operation_id });
    let next: string | undefined;
    await expect.poll(async () => {
      const current = await change({ action: 'status', operation_id: preview.operation_id });
      if (current.state === 'switched') next = current.login.token;
      return current.state;
    }).toBe('switched');
    if (!next) throw new Error('Expected a credential scoped to the target organization');
    expect((await post('/runtime/ensure-ready', next)).status).toBe(204);
    const old_page = await send(next, 'ui_tool', { instance_id, action: { type: 'poll', request_id } });
    expect(old_page.status).toBe(400);
    expect((await old_page.json()).error.code).toBe('invalid_request');
    const target_apps = await send(next, 'list_ui_apps');
    expect(target_apps.status).toBe(200);
    expect((await target_apps.json()).result.payload.payload.apps).toEqual([]);
  } finally {
    additional_organization_enabled = false;
  }
});
