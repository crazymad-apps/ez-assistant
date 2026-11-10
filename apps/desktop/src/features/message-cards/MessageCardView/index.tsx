import { useAppLinks } from '../../mcp-apps/useAppLinks';
import { useEffect, useMemo, useRef, useState } from 'react';
import { observer } from 'mobx-react-lite';
import type { ConversationOwner, MessageCardSummary } from '@ez-assistant/protocol';
import { McpUiToolResultNotificationSchema } from '@modelcontextprotocol/ext-apps';
import { McpAppView, type AppPageSource } from '../../mcp-apps';
import { useRootStore } from '../../../stores/RootStoreContext';
import { InlineIconButton } from '../../../components/InlineIconButton';
import { CardMountStore } from '../mountStore';
import styles from './index.module.scss';

export const MessageCardView = observer(function MessageCardView({
  card,
  owner,
}: {
  card: MessageCardSummary;
  owner: ConversationOwner;
}) {
  const root = useRootStore();
  const client = root.runtime_client;
  const surface = useRef<HTMLElement>(null);
  const [visible, setVisible] = useState(typeof IntersectionObserver === 'undefined');
  const [reload_height, setReloadHeight] = useState<number>();
  const owner_key = JSON.stringify(owner);
  const store = useMemo(
    () => (client ? new CardMountStore(client, JSON.parse(owner_key) as ConversationOwner, card.card_id) : null),
    [client, owner_key, card.card_id],
  );
  const source_revision = root.projection.card_sources_changed;
  const stale = root.projection.is_stale;
  useEffect(() => {
    if (!surface.current || typeof IntersectionObserver === 'undefined') return;
    const observer = new IntersectionObserver(([entry]) => setVisible(entry?.isIntersecting === true), {
      rootMargin: '400px',
    });
    observer.observe(surface.current);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    setReloadHeight(undefined);
    if (visible && !stale) void store?.load();
    return () => store?.dispose();
  }, [store, visible, stale, source_revision]);
  const mounted = store?.mounted;
  const tool_page = useMemo(() =>
    mounted?.capabilities.includes('call-tool') && client ? { client, instance_id: mounted.instance_id } : undefined,
    [mounted, client],
  );
  const source = useMemo<AppPageSource | null>(
    () =>
      mounted && client
        ? { kind: 'resource', resource: mounted.resource, proxy_url: `${client.address}/ui/sandbox-proxy` }
        : null,
    [mounted, client],
  );
  const workspace = root.resource_workspace;
  const links = useAppLinks(workspace, workspace.groups.get(`session:${owner.session_id}`), mounted?.capabilities ?? [], mounted?.instance_id);
  const result = useMemo(
    () =>
      mounted
        ? McpUiToolResultNotificationSchema.safeParse({
            method: 'ui/notifications/tool-result',
            params: mounted.tool_result,
          })
        : null,
    [mounted],
  );
  const input =
    mounted?.arguments && typeof mounted.arguments === 'object' && !Array.isArray(mounted.arguments)
      ? (mounted.arguments as Record<string, unknown>)
      : {};
  function reload() {
    const height = surface.current?.querySelector<HTMLIFrameElement>('iframe[title="应用界面"]')?.offsetHeight;
    setReloadHeight(height || undefined);
    void store?.load();
  }
  return (
    <section ref={surface} className={styles.card} aria-label={`消息卡片：${card.title}`}>
      <header className={styles.header}>
        <span>{card.title}</span>
        <span className={styles.meta}>
          {card.source.type === 'plugin' ? card.source.name : card.source.target.server_key}
        </span>
        {card.inherited && <span className={styles.meta}>继承的卡片</span>}
        <InlineIconButton
          icon="refresh"
          label="重新加载卡片"
          disabled={store?.pending}
          onClick={reload}
        />
      </header>
      {store?.pending && !mounted && (
        <p className={styles.loading} style={{ minHeight: reload_height }} role="status">正在加载卡片…</p>
      )}
      {(store?.error || stale) && <p role="alert">{store?.error ?? '连接恢复后可重新加载卡片'}</p>}
      {result && !result.success && <p role="alert">卡片数据格式不受支持，请更新插件后重新加载。</p>}
      {store?.error && (
        <button type="button" onClick={reload}>
          重新加载
        </button>
      )}
      {source && mounted && store && result?.success && (
        <McpAppView
          source={source}
          initial_height={reload_height}
          links={links}
          tool_page={tool_page}
          tool_page_current={() => root.runtime_client === client && !root.projection.is_stale &&
            root.projection.card_sources_changed === source_revision && store.mounted?.instance_id === mounted.instance_id}
          show_tool_approval
          context={{
            displayMode: 'inline',
            availableDisplayModes: ['inline'],
            platform: root.local_files.desktop ? 'desktop' : 'web',
          }}
          input={input}
          result={result.data.params}
          card={{
            revision: mounted.card.snapshot_revision,
            snapshot: mounted.snapshot,
            schema: mounted.writable_snapshot_schema,
            save: (revision, snapshot) => store.save(mounted.instance_id, revision, snapshot),
            retire: () => store.release(mounted.instance_id),
          }}
        />
      )}
      {store?.save_error && (
        <p role="alert">
          {store.save_error}
          <button type="button" onClick={reload}>
            重新加载
          </button>
        </p>
      )}
    </section>
  );
});
