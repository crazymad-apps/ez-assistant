import { expect, test } from '@playwright/test';
import { compatibilityHeaders } from '@ez-assistant/protocol';
import { createServer, type Server } from 'node:https';
import { readFile, mkdtemp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { build } from 'vite';

type Fixture = { base_url: string; access_token: string };
const fixture = (): Fixture => JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!);
const target = { source: { type: 'user' }, server_key: 'ui_fixture' };
let external: Server;
let external_origin: string;
let external_directory: string;
let harness: string;

test.beforeAll(async () => {
  external_directory = await mkdtemp(join(process.env.EZ_ASSISTANT_E2E_TEST_ROOT!, 'external-'));
  const key = join(external_directory, 'key.pem');
  const cert = join(external_directory, 'cert.pem');
  await promisify(execFile)('openssl', [
    'req',
    '-x509',
    '-newkey',
    'rsa:2048',
    '-nodes',
    '-subj',
    '/CN=localhost',
    '-addext',
    'subjectAltName=DNS:localhost,IP:127.0.0.1',
    '-keyout',
    key,
    '-out',
    cert,
    '-days',
    '1',
  ]);
  const html = await readFile('../runtime-host/tests/fixtures/mcp_ui_app.html', 'utf8');
  external = createServer({ key: await readFile(key), cert: await readFile(cert) }, (request, response) => {
    if (request.url === '/redirect') {
      response.writeHead(302, { location: `${external_origin.replace('127.0.0.1', 'localhost')}/app` });
      response.end();
      return;
    }
    response.writeHead(200, {
      'Content-Type': 'text/html',
      'Content-Security-Policy': `default-src 'none'; script-src 'unsafe-inline'; frame-ancestors ${request.url === '/deny' ? "'none'" : fixture().base_url}`,
      ...(request.url === '/deny' ? { 'X-Frame-Options': 'DENY' } : {}),
    });
    response.end(request.url === '/plain' ? '<!doctype html><p>没有通信协议</p>' : html);
  });
  await new Promise<void>((done) => external.listen(0, '127.0.0.1', done));
  const address = external.address();
  if (!address || typeof address === 'string') throw new Error('missing HTTPS fixture address');
  external_origin = `https://127.0.0.1:${address.port}`;
  const out = join(external_directory, 'bundle');
  await build({
    configFile: false,
    logLevel: 'error',
    define: { __APP_VERSION__: JSON.stringify('0.33.0'), 'process.env.NODE_ENV': JSON.stringify('production') },
    build: {
      outDir: out,
      lib: { entry: resolve('tests/support/mcp-apps/external.tsx'), formats: ['es'], fileName: () => 'harness.js' },
    },
  });
  harness = await readFile(join(out, 'harness.js'), 'utf8');
  await expect
    .poll(async () => (await command('list_mcp_ui_resources', { target })).status, { timeout: 10_000 })
    .toBe(200);
});

test.afterAll(async () => {
  if (external) await new Promise<void>((done) => external.close(() => done()));
  if (external_directory) await rm(external_directory, { recursive: true });
});

async function command(type: string, payload: object) {
  const host = fixture();
  const response = await fetch(`${host.base_url}/commands`, {
    method: 'POST',
    headers: {
      ...compatibilityHeaders(),
      Authorization: `Bearer ${host.access_token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      request_id: crypto.randomUUID(),
      command: { scope: 'runtime', payload: { type, payload } },
    }),
  });
  return response;
}

test('匿名代理固定返回独立响应策略，不开放资源或业务参数', async ({ request }) => {
  const url = `${fixture().base_url}/ui/sandbox-proxy`;
  const response = await request.get(url);
  expect(response.status()).toBe(200);
  expect(response.headers()['content-security-policy']).toContain('sandbox allow-scripts;');
  expect(response.headers()['set-cookie']).toBeUndefined();
  expect(response.headers()['cache-control']).toBe('no-store');
  expect((await request.head(url)).status()).toBe(200);
  expect((await request.get(`${url}?html=arbitrary`)).status()).toBe(400);
  expect((await request.post(url)).status()).toBe(405);
  const anonymous = await request.get(url, { headers: { Authorization: 'Bearer invalid', Cookie: 'malformed' } });
  expect(anonymous.status()).toBe(200);
  expect(await anonymous.text()).toBe(await response.text());
});

test('资源读取拒绝任意 URL、未声明 URI 与错误来源', async () => {
  expect((await command('list_mcp_ui_resources', { target })).status).toBe(200);
  for (const uri of ['https://example.invalid/', 'file:///etc/passwd', 'ui://fixture/unknown']) {
    expect((await command('read_mcp_ui_resource', { target, uri })).status).toBe(400);
  }
  expect(
    (
      await command('read_mcp_ui_resource', {
        target: { ...target, source: { type: 'plugin', name: 'wrong' } },
        uri: 'ui://fixture/card',
      })
    ).status,
  ).toBe(400);
  const visible = await (
    await command('list_mcp_server_options', {
      variant: 'build',
      context: { type: 'new_session', payload: { workspace_id: null } },
    })
  ).json();
  expect(
    visible.result.payload.payload.servers.find((server: { server_key: string }) => server.server_key === 'ui_fixture')
      .visible_tool_count,
  ).toBe(2);
});

test('正式 Host 设置页可加载隔离 UI，交互后导航撤销连接', async ({ page }) => {
  const host = fixture();
  // 普通 Web 登录兑换，随后清除 fragment；不把凭据放入插件页面。
  const issued = await fetch(`${host.base_url}/auth/login`, {
    method: 'POST',
    headers: {
      ...compatibilityHeaders(),
      Authorization: `Bearer ${host.access_token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ method: 'desktop' }),
  });
  const { token } = (await issued.json()) as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.getByRole('button', { name: '插件管理' })).toBeVisible();
  await page.getByRole('button', { name: '账号与 Runtime' }).click();
  await page.getByRole('menuitem', { name: '设置', exact: true }).click();
  await page.getByRole('navigation', { name: '设置页面' }).getByRole('button', { name: 'MCP', exact: true }).click();
  await page.getByRole('button', { name: /ui_fixture/ }).click();
  await page.getByRole('button', { name: '界面资源', exact: true }).click();
  await page.getByRole('button', { name: '通用界面', exact: true }).click();
  const view = page.getByRole('region', { name: '应用页面' });
  await expect(view).toHaveAttribute('data-app-state', 'connected');
  const app = page.frameLocator('iframe[title="应用界面"]').frameLocator('iframe[title="应用内容"]');
  await expect(app.getByRole('heading', { name: '标准 MCP 页面' })).toBeVisible();
  await expect(app.locator('body')).toHaveAttribute('data-parent-readable', 'false');
  await expect(app.locator('body')).toHaveAttribute('data-cookie-readable', 'false');
  await expect(app.locator('body')).toHaveAttribute('data-network', 'blocked');
  await expect(app.locator('body')).toHaveAttribute('data-host-capabilities', '{}');
  await app.getByRole('button', { name: '增加' }).click();
  await expect(app.getByText('累计 1', { exact: true })).toBeVisible();
  await app.getByRole('button', { name: '导航', exact: true }).click();
  await expect(view).toHaveAttribute('data-app-state', 'error');
  await expect(view.locator('iframe')).toHaveCount(0);
});

test('外部 HTTPS 页面使用真实来源握手，跳转、关闭和不支持通信可解释', async ({ browser }) => {
  test.setTimeout(45_000);
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  try {
    const page = await context.newPage();
    const origin = fixture().base_url;
    await page.route(`${origin}/__f03__/harness.js`, (route) =>
      route.fulfill({ contentType: 'application/javascript', body: harness }),
    );
    await page.goto(origin);
    await page.addScriptTag({ url: `${origin}/__f03__/harness.js`, type: 'module' });
    await page.waitForFunction(() => typeof window.f03Mount === 'function');
    const mount = async (path: string) =>
      page.evaluate(
        ({ url, approved, host }) =>
          window.f03Mount({ kind: 'external', url, approved_origin: approved, host_origins: [host] }),
        { url: `${external_origin}${path}`, approved: external_origin, host: origin },
      );
    const private_logs: string[] = [];
    page.on('console', (message) => {
      if (message.text().includes('fixture-ui-private')) private_logs.push(message.text());
    });
    await mount('/app');
    const view = page.getByRole('region', { name: '应用页面' });
    await expect(view).toHaveAttribute('data-app-state', 'connected');
    await expect(
      page.frameLocator('iframe[title="应用界面"]').getByRole('heading', { name: '标准 MCP 页面' }),
    ).toBeVisible();
    await expect(page.frameLocator('iframe[title="应用界面"]').locator('body')).toHaveAttribute(
      'data-cookie-readable',
      'true',
    );
    const app_body = page.frameLocator('iframe[title="应用界面"]').locator('body');
    await expect(app_body).toHaveAttribute('data-input', 'fixture-input');
    await expect(app_body).toHaveAttribute('data-result', 'fixture-result');
    await expect(app_body).toHaveAttribute('data-private-result', 'fixture-ui-private');
    expect(private_logs).toEqual([]);
    await page.evaluate(() => window.f03Unmount());
    await expect(page.locator('iframe[title="应用界面"]')).toHaveCount(0);
    await mount('/redirect');
    await expect(view).toHaveAttribute('data-app-state', 'error');
    await expect(view.locator('iframe')).toHaveCount(0);
    await mount('/plain');
    await expect(view).toHaveAttribute('data-app-state', 'error', { timeout: 15_000 });
    await expect(view).toContainText('可能不支持通信或限制嵌入');
    await mount('/deny');
    await expect(view).toHaveAttribute('data-app-state', 'error', { timeout: 15_000 });
    await expect(view.locator('iframe')).toHaveCount(0);
    await page.evaluate(
      ({ host }) =>
        window.f03Mount({ kind: 'external', url: `${host}/auth/login`, approved_origin: host, host_origins: [host] }),
      { host: origin },
    );
    await expect(view).toContainText('应用来源未获授权');
  } finally {
    await context.close();
  }
});
