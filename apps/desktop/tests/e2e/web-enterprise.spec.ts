import { test, expect, type Page } from '@playwright/test';
import { createServer } from 'node:http';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { compatibilityHeaders } from '@ez-assistant/protocol';

// 可控 Center 只维护测试内存凭据；正式 Host 使用全新临时 Home，无共享中心或用户数据。
const tokens = new Map<string, number>();
let serial = 0,
  logout_count = 0,
  home = '',
  address = '',
  management = '';
let host: ChildProcess;
let managed_model: object | null = null;
let model_refresh_fails = false;
const identity = (id: number) => ({
  center_id: '01234567-89ab-4cde-8f01-23456789abcd',
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
      JSON.stringify({ protocol_version: 1, min_protocol_version: 1, capabilities: ['managed_models', 'llm_proxy'] }),
    );
  if (request.url === '/api/auth/login' && body.password === 'fixture-password') {
    const id = body.username === 'alice' ? 1 : 2;
    const suffix = (++serial).toString(16).padStart(64, '0');
    tokens.set(`ct_${suffix}`, id);
    return response.end(JSON.stringify({ ...identity(id), token: `ct_${suffix}`, llm_key: `cl_${suffix}` }));
  }
  if (request.url === '/api/runtime/model-configuration' && id) {
    if (model_refresh_fails) {
      response.statusCode = 503;
      return response.end('{}');
    }
    return response.end(
      JSON.stringify(
        managed_model ?? { state: 'unavailable', center_id: identity(id).center_id, reason: 'not_selected' },
      ),
    );
  }
  if (request.url === '/api/auth/me' && id) return response.end(JSON.stringify(identity(id)));
  if (request.url === '/api/auth/password' && id) {
    response.statusCode = 204;
    return response.end();
  }
  if (request.url === '/api/auth/logout' && id) {
    tokens.delete(token);
    logout_count++;
    response.statusCode = 204;
    return response.end();
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
  if (home) await rm(home, { recursive: true, force: true });
});
async function login(page: Page, username: string) {
  await page.goto(address);
  await page.getByLabel('企业账号', { exact: true }).fill(username);
  await page.getByLabel('账号密码', { exact: true }).fill('fixture-password');
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page.locator('[data-app-title-bar]')).toBeVisible();
}
async function logout(page: Page, user: string, cancel = false) {
  await page.getByRole('button', { name: user, exact: true }).click();
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
    await expect(page.getByRole('heading', { name: '连接你的工作空间' })).toHaveCount(0);
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
  await page.getByRole('button', { name: '进入工作空间' }).click();
  await expect(page.getByRole('button', { name: '重试初始化' })).toBeVisible();
  expect(readiness).toBe(1);
  await page.getByRole('button', { name: '重试初始化' }).click();
  await expect(page.getByRole('button', { name: 'Alice', exact: true })).toBeVisible();
  await login(bob, 'bob');
  await page.getByRole('button', { name: 'Alice', exact: true }).click();
  await expect(page.getByRole('menuitem')).toHaveText(['修改密码', '退出登录']);
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
  await expect(page.getByRole('button', { name: 'Alice', exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByRole('button', { name: 'Alice', exact: true })).toBeVisible();
  await logout(page, 'Alice');
  await expect(page.getByLabel('企业账号', { exact: true })).toBeVisible();
  await expect(bob.getByRole('button', { name: 'Bob', exact: true })).toBeVisible();
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
  await expect(second.getByRole('button', { name: 'Alice', exact: true })).toBeVisible();
  const issued = await fetch(`${address}/auth/login`, {
    method: 'POST',
    headers: { ...compatibilityHeaders(), 'Content-Type': 'application/json' },
    body: JSON.stringify({ method: 'enterprise', username: 'bob', password: 'fixture-password', native: true }),
  });
  const { token } = (await issued.json()) as { token: string };
  await second.goto(`${address}/#token=${token}`);
  await expect(second.getByRole('button', { name: 'Bob', exact: true })).toBeVisible();
  await first.bringToFront();
  await expect(first.getByRole('button', { name: 'Bob', exact: true })).toBeVisible();
  expect(new URL(second.url()).hash).toBe('');
  expect(await first.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toMatch(
    /ct_|cl_|session:|login_context/,
  );
  await context.close();
});

test('管理配置保存后当前模式保持冻结并要求重启', async () => {
  // 本用例自行建立中心绑定，不依赖前面浏览器用例的执行顺序。
  const login = await fetch(`${address}/auth/login`, {
    method: 'POST',
    headers: { ...compatibilityHeaders(), 'Content-Type': 'application/json' },
    body: JSON.stringify({ method: 'enterprise', username: 'alice', password: 'fixture-password', native: true }),
  });
  expect(login.status).toBe(200);
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
  const configuration = (model_id: string) => ({
    state: 'ready',
    center_id: identity(1).center_id,
    endpoint: `http://127.0.0.1:${port}/api/llm/providers/${identity(1).center_id}/v1`,
    provider_type: 'openai',
    provider_display_name: '企业测试服务商',
    protocol: 'open_ai_chat_completions',
    model_id,
    parameters,
  });
  managed_model = configuration('center-model-a');
  await login(page, 'alice');
  await page.getByRole('button', { name: '模型设置', exact: true }).click();
  await page.getByRole('menuitem', { name: '企业测试服务商', exact: true }).click();
  await expect(page.getByRole('menuitemradio', { name: 'center-model-a', exact: true })).toBeDisabled();
  await page.screenshot({ path: info.outputPath('managed-model-menu.png'), animations: 'disabled' });
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '设置', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '设置', exact: true });
  await dialog.getByRole('button', { name: '模型', exact: true }).click();
  await expect(dialog.getByRole('button', { name: '默认模型', exact: true })).toContainText('center-model-a');
  await expect(dialog.getByRole('button', { name: '添加服务商' })).toHaveCount(0);
  managed_model = configuration('center-model-b');
  await dialog.getByRole('button', { name: '刷新配置' }).click();
  await expect(dialog.getByRole('button', { name: '默认模型', exact: true })).toContainText('center-model-b');
  await dialog.getByRole('button', { name: /管理服务商 企业测试服务商/ }).click();
  await expect(dialog.getByRole('button', { name: '添加模型' })).toHaveCount(0);
  await dialog.getByRole('button', { name: '配置模型 center-model-b', exact: true }).click();
  await expect(dialog.getByLabel('上下文窗口（Token）', { exact: true })).toBeDisabled();
  await expect(dialog.getByLabel('上下文窗口（Token）', { exact: true })).toHaveValue('8192');
  await page.screenshot({ path: info.outputPath('managed-model-ready.png'), animations: 'disabled' });
  await dialog.getByRole('button', { name: '返回', exact: true }).click();
  await dialog.getByRole('button', { name: '返回', exact: true }).click();
  model_refresh_fails = true;
  await dialog.getByRole('button', { name: '刷新配置' }).click();
  await expect(dialog.getByRole('button', { name: '默认模型', exact: true })).toContainText('center-model-b');
  await expect(dialog.getByText(/^刷新失败：/)).toBeVisible();
  model_refresh_fails = false;
  managed_model = null;
  await dialog.getByRole('button', { name: '刷新配置' }).click();
  await expect(dialog.getByText('管理员尚未配置默认模型。', { exact: true })).toBeVisible();
  await expect(dialog.getByText('center-model-b', { exact: true })).toHaveCount(0);
  await page.screenshot({ path: info.outputPath('managed-model-unavailable.png'), animations: 'disabled' });
});
