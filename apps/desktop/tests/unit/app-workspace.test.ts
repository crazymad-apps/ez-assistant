import { AppController } from '../../src/features/resource-workspace/AppController';
import { AppLinkEndpoint } from '../../src/features/mcp-apps/appLinks';
import { expect, it, vi } from 'vitest';
import type { RuntimeClient } from '../../src/runtime-client/RuntimeClient';
import { ResourceWorkspaceStore, resourceTabKey } from '../../src/features/resource-workspace/ResourceWorkspaceStore';
import { parseResourceSnapshot } from '../../src/features/resource-workspace/resourceWorkspaceSnapshot';

const client = { address: 'https://host.invalid' } as RuntimeClient;
it('reuses an app only inside the same owner and instance key, while retaining browser tabs', () => {
  const workspace = new ResourceWorkspaceStore(() => client);
  workspace.selectScope('session:one');
  const target = { type: 'external' as const, url: 'https://app.invalid/' };
  const first = workspace.openApp(target, { private: 'initial' });
  expect(workspace.openApp(target, { private: 'new' })).toBe(first);
  expect(workspace.openApp(target, {}, 'another')).not.toBe(first);
  workspace.openBrowser();
  expect(workspace.tabs.map((t) => t.type)).toEqual(['context', 'app', 'app', 'browser']);
  workspace.selectScope('session:two');
  expect(workspace.openApp(target)).not.toBe(first);
  workspace.dispose();
});
it('saves only the entry and never the active HTML, private arguments or approval', async () => {
  const workspace = new ResourceWorkspaceStore(() => client);
  const app = workspace.openApp({ type: 'external', url: 'https://app.invalid/' }, { secret: 'private-state' });
  await app.load(true);
  expect(app.source).not.toBeNull();
  const saved = workspace.captureSnapshot();
  expect(JSON.stringify(saved)).not.toContain('private-state');
  expect(JSON.stringify(saved)).not.toContain('approved_origin');
  const restored = new ResourceWorkspaceStore(() => client);
  await restored.restoreSnapshot(parseResourceSnapshot(saved)!);
  expect(restored.apps.size).toBe(1);
  expect([...restored.apps.values()][0].source).toBeNull();
  expect([...restored.apps.values()][0].arguments_value).toEqual({});
  workspace.dispose();
  restored.dispose();
});
it('keeps apps inside the existing cache budget and releases them with their group', () => {
  const workspace = new ResourceWorkspaceStore(() => client);
  workspace.selectScope('session:one');
  for (let n = 0; n < 22; n++) workspace.openApp({ type: 'plugin', name: 'fixture', entry_id: `app-${n}` });
  expect(workspace.cached_pages.size).toBe(20);
  workspace.releaseScope('session:one');
  expect(workspace.apps.size).toBe(0);
  expect(workspace.cached_pages.size).toBe(0);
  workspace.dispose();
});

it('revalidates every locate, retains a warm page, and retires it when the source changes', async () => {
  const payload = {
    source_identity: 'source-one',
    entry: { title: 'App', capabilities: ['post-message'] },
    resource: { uri: 'ui://test/app', html: '<p>test</p>', ui: null },
  };
  const command = vi.fn().mockResolvedValue({ payload });
  const client = { address: 'https://host.invalid', command } as unknown as RuntimeClient;
  const app = new AppController({ type: 'plugin', name: 'fixture', entry_id: 'app' }, undefined, {}, () => client);
  await app.load();
  const source = app.source;
  await app.locate({ count: 2 });
  expect(app.source).toBe(source);
  expect(command.mock.calls[1][0].payload.arguments).toEqual({ count: 2 });
  command.mockRejectedValueOnce(new Error('invalid input'));
  await expect(app.locate({ count: 'invalid' })).rejects.toThrow();
  expect(app.source).toBe(source);
  const endpoint = new AppLinkEndpoint(['post-message'], vi.fn(), vi.fn());
  app.attach(endpoint);
  command.mockResolvedValueOnce({ payload: { ...payload, source_identity: 'source-two' } });
  await app.locate({ count: 3 });
  expect(endpoint.live).toBe(false);
  expect(app.source).not.toBe(source);
  app.disconnect();
});

it('promotes an app in LRU without moving its mounted page', () => {
  const workspace = new ResourceWorkspaceStore(() => client);
  workspace.openApp({ type: 'plugin', name: 'fixture', entry_id: 'first' });
  const first = resourceTabKey(workspace.active_tab);
  workspace.openApp({ type: 'plugin', name: 'fixture', entry_id: 'second' });
  const mounted = workspace.mounted_pages.map((page) => page.key);
  workspace.activateTab(first);
  expect(workspace.mounted_pages.map((page) => page.key)).toEqual(mounted);
  expect([...workspace.cached_pages.values()].at(-1)?.tab).toBe(workspace.active_tab);
  workspace.dispose();
});
