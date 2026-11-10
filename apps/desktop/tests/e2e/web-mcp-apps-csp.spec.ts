import { readFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

type Fixture = Readonly<{ base_url: string }>;

function hostOrigin(): string {
  const fixture = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP ?? 'null') as Fixture | null;
  if (!fixture) throw new Error('需要显式启动的隔离 Host');
  return fixture.base_url;
}

const proxy_ready = 'ui/notifications/sandbox-proxy-ready';
const proxy_html = `<!doctype html><meta http-equiv="Content-Security-Policy" content="script-src 'unsafe-inline'"><script>parent.postMessage({jsonrpc:'2.0',method:'${proxy_ready}',params:{}},'*')</script>`;

async function mountDataProxy(page: Page): Promise<void> {
  await page.evaluate(
    ({ html, method }) => {
      const frame = document.createElement('iframe');
      frame.title = 'F03 隔离承载验证';
      frame.sandbox.add('allow-scripts', 'allow-same-origin');
      // 只接受当前 frame 的通知；null 来源不能代替实例身份。
      window.addEventListener('message', (event) => {
        if (event.source === frame.contentWindow && event.data?.method === method) {
          document.body.dataset.f03ProxyReady = 'true';
        }
      });
      frame.src = `data:text/html;base64,${btoa(html)}`;
      document.body.append(frame);
    },
    { html: proxy_html, method: proxy_ready },
  );
}

test('正式 Host CSP 拒绝 data 代理且主应用不开放内联脚本', async ({ page }) => {
  const response = await page.goto(hostOrigin());
  expect(response?.status()).toBe(200);
  const policy = response?.headers()['content-security-policy'] ?? '';
  const directives = policy.split(';').map((value) => value.trim());
  expect(directives.find((value) => value.startsWith('script-src '))).toBe("script-src 'self'");
  expect(directives.find((value) => value.startsWith('frame-src '))).not.toContain('data:');
  const blocked = page.waitForEvent('console', {
    predicate: (message) => message.type() === 'error' && /frame-src/.test(message.text()),
  });
  await mountDataProxy(page);
  await blocked;
  await expect(page.locator('body')).not.toHaveAttribute('data-f03-proxy-ready');
});

test('仅放行 data frame 或添加子页面 meta 不能解除继承的脚本策略', async ({ page }) => {
  // 仅修改测试浏览器收到的主文档响应，不改变产品 CSP 或 Host 配置。
  await page.route(`${hostOrigin()}/`, async (route) => {
    const response = await route.fetch();
    const headers = response.headers();
    const policy = headers['content-security-policy'];
    expect(policy).toMatch(/frame-src [^;]*blob:/);
    await route.fulfill({
      response,
      headers: {
        ...headers,
        'content-security-policy': policy.replace(/frame-src ([^;]+)/, 'frame-src $1 data:'),
      },
    });
  });
  await page.goto(hostOrigin());
  const blocked = page.waitForEvent('console', {
    predicate: (message) => message.type() === 'error' && /inline script/.test(message.text()),
  });
  await mountDataProxy(page);
  await blocked;
  await expect(page.locator('body')).not.toHaveAttribute('data-f03-proxy-ready');
});

test('Desktop 配置也在 frame 边界拒绝 data 代理', async ({ page }) => {
  const configuration = JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8')) as {
    app: { security: { csp: string } };
  };
  // Chromium 仅核验配置策略，不将此结果冒充原生 WebView 验收。
  await page.route(`${hostOrigin()}/`, async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      headers: {
        ...response.headers(),
        'content-security-policy': configuration.app.security.csp,
      },
    });
  });
  await page.goto(hostOrigin());
  const blocked = page.waitForEvent('console', {
    predicate: (message) => message.type() === 'error' && /frame-src/.test(message.text()),
  });
  await mountDataProxy(page);
  await blocked;
  await expect(page.locator('body')).not.toHaveAttribute('data-f03-proxy-ready');
});
