import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import type { ConversationOwner, MessageCardSummary } from '@ez-assistant/protocol';
import { MessageCards } from '../../src/features/message-cards/MessageCards';

const { command, root } = vi.hoisted(() => {
  const command = vi.fn();
  return {
    command,
    root: {
      runtime_client: { command, ui_client_instance_id: 'client' },
      projection: { message_cards_changed: 0, is_stale: false },
    },
  };
});
vi.mock('../../src/stores/RootStoreContext', () => ({
  useRootStore: () => root,
}));
vi.mock('../../src/features/message-cards/MessageCardView', () => ({
  MessageCardView: ({ card }: { card: MessageCardSummary }) => <p>{card.title}</p>,
}));
afterEach(() => {
  cleanup();
  command.mockReset();
});
const owner: ConversationOwner = { type: 'main_session', session_id: 'owner-one' };
const card = (id: string) => ({ card_id: id, title: id });

it('loads the next cursor and appends cards without repeating the previous page', async () => {
  command
    .mockResolvedValueOnce({ payload: { cards: [card('first')], next: 1 } })
    .mockResolvedValueOnce({ payload: { cards: [card('first'), card('second')], next: null } });
  render(<MessageCards owner={owner} run_id="run-one" message_ids={[]} />);
  expect(await screen.findByText('first')).toBeVisible();
  fireEvent.click(screen.getByRole('button', { name: '加载更多卡片' }));
  expect(await screen.findByText('second')).toBeVisible();
  expect(screen.getAllByText('first')).toHaveLength(1);
  expect(command.mock.calls[1][0].payload.after).toBe(1);
  expect(screen.queryByRole('button', { name: '加载更多卡片' })).toBeNull();
});

it('a failed query after changing owner cannot expose the previous owner cards', async () => {
  command
    .mockResolvedValueOnce({ payload: { cards: [card('private-title')], next: null } })
    .mockRejectedValueOnce(new Error('unavailable'));
  const { rerender } = render(<MessageCards owner={owner} run_id="run-one" message_ids={[]} />);
  expect(await screen.findByText('private-title')).toBeVisible();
  rerender(
    <MessageCards owner={{ type: 'main_session', session_id: 'owner-two' }} run_id="run-two" message_ids={[]} />,
  );
  expect(await screen.findByRole('alert')).toHaveTextContent('卡片读取失败');
  expect(screen.queryByText('private-title')).toBeNull();
});
