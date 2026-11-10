import { describe, expect, it, vi } from 'vitest';
import { AppLinkEndpoint, normalizeAppTarget } from '../../src/features/mcp-apps/appLinks';

describe('page associations', () => {
  it('only the paired endpoints can use a handle and retirement revokes both directions', async () => {
    const receive = vi.fn().mockResolvedValue(undefined);
    const peer = new AppLinkEndpoint(['post-message'], vi.fn(), vi.fn());
    peer.connect(receive);
    const caller = new AppLinkEndpoint(['open-app', 'post-message'], async () => peer, vi.fn());
    const caller_receive = vi.fn().mockResolvedValue(undefined);
    caller.connect(caller_receive);
    const response = (await caller.request('ez-assistant/open-app', {
      target: { type: 'plugin', name: 'test', entry_id: 'app' },
      arguments: { item: 1 },
    })) as { handle: string };
    expect(receive).toHaveBeenCalledWith('ez-assistant/notifications/link-opened', {
      handle: response.handle,
      arguments: { item: 1 },
    });
    const intruder = new AppLinkEndpoint(['post-message'], vi.fn(), vi.fn());
    intruder.connect(vi.fn());
    await expect(
      intruder.request('ez-assistant/post-message', { handle: response.handle, payload: 'wrong' }),
    ).rejects.toThrow();
    await peer.request('ez-assistant/post-message', { handle: response.handle, payload: { result: 2 } });
    expect(caller_receive).toHaveBeenCalledWith('ez-assistant/notifications/link-message', {
      handle: response.handle,
      payload: { result: 2 },
    });
    caller.retire();
    await expect(
      peer.request('ez-assistant/post-message', { handle: response.handle, payload: 'late' }),
    ).rejects.toThrow();
    expect(receive).toHaveBeenLastCalledWith('ez-assistant/notifications/link-closed', { handle: response.handle });
    peer.retire();
    intruder.retire();
  });
  it('does not create a connection if the opener retires while the app is loading', async () => {
    let finish!: (peer: AppLinkEndpoint) => void;
    const opener = new AppLinkEndpoint(
      ['open-app'],
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
      vi.fn(),
    );
    opener.connect(vi.fn());
    const pending = opener.request('ez-assistant/open-app', {
      target: { type: 'plugin', name: 'test', entry_id: 'app' },
      arguments: {},
    });
    opener.retire();
    const peer = new AppLinkEndpoint(['post-message'], vi.fn(), vi.fn());
    const notify = vi.fn();
    peer.connect(notify);
    finish(peer);
    await expect(pending).rejects.toThrow();
    expect(notify).not.toHaveBeenCalled();
    peer.retire();
  });
  it('accepts HTTP and HTTPS external URLs while rejecting Host origins and embedded credentials', () => {
    const host_origins = ['http://host.invalid', 'https://host.invalid'];
    for (const scheme of ['http', 'https']) {
      const app = { type: 'external', url: `${scheme}://app.invalid/` } as const;
      expect(normalizeAppTarget(app, host_origins)).toEqual(app);
      for (const url of [`${scheme}://host.invalid/api`, `${scheme}://name:password@app.invalid/`])
        expect(() => normalizeAppTarget({ type: 'external', url }, host_origins)).toThrow();
    }
    expect(() => normalizeAppTarget({ type: 'external', url: 'ftp://app.invalid/' }, host_origins)).toThrow();
  });
});

it('retires pending draft confirmation along with its source page', async () => {
  let aborted = false;
  const source = new AppLinkEndpoint(
    ['set-composer-draft'],
    vi.fn(),
    async (_text, _active, signal) =>
      new Promise((_resolve, reject) =>
        signal.addEventListener(
          'abort',
          () => {
            aborted = true;
            reject(new Error('retired'));
          },
          { once: true },
        ),
      ),
  );
  source.connect(vi.fn());
  const pending = source.request('ez-assistant/set-composer-draft', { text: 'draft' });
  source.retire();
  await expect(pending).rejects.toThrow('retired');
  expect(aborted).toBe(true);
});

it('checks the current source before handling a message, independently of React cleanup', async () => {
  let current = true;
  const fill = vi.fn().mockResolvedValue(undefined);
  const source = new AppLinkEndpoint(['set-composer-draft'], vi.fn(), fill, () => current);
  source.connect(vi.fn());
  current = false;
  await expect(source.request('ez-assistant/set-composer-draft', { text: 'late' })).rejects.toThrow();
  expect(fill).not.toHaveBeenCalled();
  source.retire();
});
