import { describe, expect, it, vi } from 'vitest';
import type { MountMessageCardResult, ConversationOwner } from '@ez-assistant/protocol';
import type { RuntimeClient } from '../../src/runtime-client/RuntimeClient';
import { CardMountStore } from '../../src/features/message-cards/mountStore';

const owner: ConversationOwner = { type: 'main_session', session_id: 'session-card' };
function mounted(id: string): MountMessageCardResult {
  return {
    instance_id: id,
    card: {
      card_id: 'card-one',
      owner,
      run_id: 'run-one',
      anchor_message_id: 'message-one',
      title: 'Counter',
      source: { type: 'plugin', name: 'fixture', entry_id: 'counter', published_sha256: 'a'.repeat(64) },
      position: 1,
      snapshot_revision: 0,
      inherited: false,
    },
    resource: { uri: 'ui://fixture/card', html: '<p>test</p>', ui: null },
    arguments: {},
    tool_result: { content: [] },
    snapshot: null,
    writable_snapshot_schema: 'v1',
    capabilities: [],
  };
}

describe('card mount projection', () => {
  it('late mount after disposal is released and cannot restore the old page', async () => {
    let complete!: (value: unknown) => void;
    const command = vi.fn().mockImplementation((request) =>
      request.type === 'mount_message_card'
        ? new Promise((resolve) => {
            complete = resolve;
          })
        : Promise.resolve({ payload: null }),
    );
    const store = new CardMountStore(
      { command, ui_client_instance_id: 'client' } as unknown as RuntimeClient,
      owner,
      'card-one',
    );
    const load = store.load();
    store.dispose();
    complete({ payload: mounted('old') });
    await load;
    expect(store.mounted).toBeNull();
    expect(command).toHaveBeenCalledWith({
      type: 'release_message_card',
      payload: { owner, card_id: 'card-one', instance_id: 'old' },
    });
  });

  it('snapshot conflicts preserve the mounted value and use its bound card and instance', async () => {
    const command = vi
      .fn()
      .mockImplementation((request) =>
        Promise.resolve(
          request.type === 'mount_message_card'
            ? { payload: mounted('active') }
            : { payload: { saved: false, revision: 2, snapshot: { schema: 'v1', value: { count: 9 } } } },
        ),
      );
    const store = new CardMountStore(
      { command, ui_client_instance_id: 'client' } as unknown as RuntimeClient,
      owner,
      'card-one',
    );
    await store.load();
    const result = await store.save('active', 0, { schema: 'v1', value: { count: 1 } });
    expect(result.saved).toBe(false);
    expect(store.save_error).toContain('其他页面更新');
    expect(store.mounted?.snapshot).toBeNull();
    expect(command).toHaveBeenLastCalledWith({
      type: 'save_message_card_snapshot',
      payload: {
        owner,
        card_id: 'card-one',
        instance_id: 'active',
        expected_revision: 0,
        snapshot: { schema: 'v1', value: { count: 1 } },
      },
    });
    await expect(store.save('retired', 2, { schema: 'v1', value: null })).rejects.toThrow();
    store.dispose();
  });
});
