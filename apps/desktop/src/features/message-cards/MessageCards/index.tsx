import { useCallback, useEffect, useRef, useState } from 'react';
import { observer } from 'mobx-react-lite';
import type { ConversationOwner, MessageCardSummary } from '@ez-assistant/protocol';
import { useRootStore } from '../../../stores/RootStoreContext';
import { MessageCardView } from '../MessageCardView';
import styles from './index.module.scss';

/** 只查询已显示轮次；同一 Run 的最后一个消息段由 ConversationView 提供唯一挂载位置。 */
export const MessageCards = observer(function MessageCards({
  owner,
  run_id,
  message_ids,
}: {
  owner: ConversationOwner;
  run_id: string | null;
  message_ids: readonly string[];
}) {
  const root = useRootStore();
  const client = root.runtime_client;
  const revision = root.projection.message_cards_changed;
  const stale = root.projection.is_stale;
  const key = JSON.stringify([owner, run_id, message_ids, client?.ui_client_instance_id]);
  const [state, setState] = useState<{ key: string; cards: MessageCardSummary[]; next: number | null; error: boolean }>(
    { key, cards: [], next: null, error: false },
  );
  const [page, setPage] = useState(0);
  const active_request = useRef<AbortController | null>(null);
  const load = useCallback(
    (after: number | null) => {
      if (!client || stale) return;
      active_request.current?.abort();
      const request = new AbortController();
      active_request.current = request;
      const [target, run, messages] = JSON.parse(key) as [ConversationOwner, string | null, string[]];
      void client
        .command(
          {
            type: 'list_message_cards',
            payload: { owner: target, run_ids: run ? [run] : [], message_ids: messages, after, limit: 100 },
          },
          { signal: request.signal },
        )
        .then((result) => {
          if (!Array.isArray(result.payload.cards)) throw new Error('卡片响应无效');
          if (!request.signal.aborted)
            setState((old) => ({
              key,
              cards:
                after !== null && old.key === key
                  ? [
                      ...old.cards,
                      ...result.payload.cards.filter(
                        (card) => !old.cards.some((previous) => previous.card_id === card.card_id),
                      ),
                    ]
                  : result.payload.cards,
              next: result.payload.next,
              error: false,
            }));
        })
        .catch(() => {
          if (!request.signal.aborted)
            setState((old) => ({
              key,
              cards: old.key === key ? old.cards : [],
              next: old.key === key ? old.next : null,
              error: true,
            }));
        });
    },
    [client, key, stale],
  );
  useEffect(() => {
    load(null);
    return () => active_request.current?.abort();
  }, [load, revision, page]);
  const cards = state.key === key ? state.cards : [];
  const error = state.key === key && state.error;
  if (!cards.length && !error) return null;
  return (
    <div className={styles.cards}>
      {cards.map((card) => (
        <MessageCardView key={card.card_id} card={card} owner={owner} />
      ))}
      {error && (
        <p role="alert">
          卡片读取失败。
          <button type="button" onClick={() => setPage((value) => value + 1)}>
            重试
          </button>
        </p>
      )}
      {state.key === key && state.next !== null && (
        <button type="button" onClick={() => load(state.next)}>
          加载更多卡片
        </button>
      )}
    </div>
  );
});
