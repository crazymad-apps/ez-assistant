import { observer } from 'mobx-react-lite';
import { useEffect, useMemo } from 'react';
import type { McpServerSnapshot } from '@ez-assistant/protocol';
import { useRootStore } from '../../../../../stores/RootStoreContext';
import { McpAppView, type AppPageSource } from '../../../../mcp-apps';
import { UiResourcesStore } from './store';
import styles from './index.module.scss';

const context = {
  displayMode: 'inline' as const,
  availableDisplayModes: ['inline' as const],
  locale: navigator.language,
};

export const UiResources = observer(function UiResources(props: Readonly<{ server: McpServerSnapshot }>) {
  const root = useRootStore();
  const client = root.local_runtime_client;
  const source_key = JSON.stringify(props.server.source);
  const server_key = props.server.server_key;
  const store = useMemo(
    () =>
      client
        ? new UiResourcesStore(client, { source: JSON.parse(source_key) as McpServerSnapshot['source'], server_key })
        : null,
    [client, source_key, server_key],
  );
  const application = root.local_projection.application;
  // 配置/目录快照换代或断线时保守撤销预览；不让旧 HTML 延续到新连接/插件。
  useEffect(() => {
    store?.dispose();
    return () => store?.dispose();
  }, [store, application]);
  const resource = store?.resource;
  const source = useMemo<AppPageSource | null>(
    () => (resource && client ? { kind: 'resource', resource, proxy_url: `${client.address}/ui/sandbox-proxy` } : null),
    [resource, client],
  );
  const connected = ['connected', 'connected_without_tools'].includes(props.server.runtime_state);
  if (!store || !connected || props.server.shadowed_by || props.server.needs_refresh || root.local_projection.is_stale)
    return null;

  return (
    <section className={styles.resources} aria-label="MCP 界面资源">
      <button type="button" disabled={store.pending} onClick={() => void store.load()}>
        界面资源
      </button>
      {store.pending && <p role="status">正在读取界面资源…</p>}
      {store.error && <p role="alert">{store.error}</p>}
      {store.resources?.length === 0 && <p>没有界面资源</p>}
      <div className={styles.entries}>
        {store.resources?.map((item) => (
          <button type="button" disabled={store.pending} key={item.uri} onClick={() => void store.read(item.uri)}>
            {item.title ?? item.name}
          </button>
        ))}
      </div>
      {source && (
        <McpAppView source={source} context={{ ...context, platform: root.local_files.desktop ? 'desktop' : 'web' }} />
      )}
    </section>
  );
});
