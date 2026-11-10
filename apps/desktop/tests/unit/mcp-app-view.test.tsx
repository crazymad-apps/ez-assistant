import { StrictMode } from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { McpAppView, type AppPageSource } from '../../src/features/mcp-apps';
import { AppLinkEndpoint } from '../../src/features/mcp-apps/appLinks';

beforeEach(() => {
  Object.defineProperty(HTMLIFrameElement.prototype, 'sandbox', {
    configurable: true,
    value: { add() {} },
  });
});

afterEach(() => {
  Reflect.deleteProperty(HTMLIFrameElement.prototype, 'sandbox');
});

it('keeps the card and link alive when development mode replays the mount effect', async () => {
  const links = new AppLinkEndpoint(['post-message'], vi.fn(), vi.fn());
  const retire = vi.fn();
  const source: AppPageSource = {
    kind: 'resource',
    proxy_url: 'http://127.0.0.1:7322/ui/sandbox-proxy',
    resource: { uri: 'ui://counter/card', html: '<p>Counter</p>', ui: null },
  };
  const view = render(
    <StrictMode>
      <McpAppView
        source={source}
        context={{ displayMode: 'inline' }}
        links={links}
        input={{ count: 42 }}
        card={{ schema: 'v1', revision: 0, snapshot: null, save: vi.fn(), retire }}
      />
    </StrictMode>,
  );
  const frame = await screen.findByTitle<HTMLIFrameElement>('应用界面');
  const target = frame.contentWindow!;
  const post = vi.spyOn(target, 'postMessage').mockImplementation(() => {});
  const receive = (data: object) => window.dispatchEvent(new MessageEvent('message', {
    origin: 'null', source: target, data: { jsonrpc: '2.0', ...data },
  }));
  await act(async () => {
    receive({
      method: 'ui/initialize', id: 1,
      params: { appInfo: { name: 'Counter', version: '1.0.0' }, appCapabilities: {}, protocolVersion: '2026-01-26' },
    });
  });
  await waitFor(() => expect(post).toHaveBeenCalledWith(expect.objectContaining({ id: 1, result: expect.any(Object) }), '*'));
  await act(async () => receive({ method: 'ui/notifications/initialized', params: {} }));
  await expect(links.ready()).resolves.toBeUndefined();
  expect(screen.getByRole('region', { name: '应用页面' })).toHaveAttribute('data-app-state', 'connected');
  expect(post).toHaveBeenCalledWith(expect.objectContaining({
    method: 'ui/notifications/tool-input', params: { arguments: { count: 42 } },
  }), '*');
  expect(retire).not.toHaveBeenCalled();

  view.unmount();
  expect(links.live).toBe(false);
  expect(retire).toHaveBeenCalledTimes(1);
});
