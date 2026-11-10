import { useEffect, useState } from 'react';
import type { UiAppSummary } from '@ez-assistant/protocol';
import { Dialog } from '../../components/Dialog';
import { Button } from '../../components/Button';
import type { ResourceTabGroup, ResourceWorkspaceStore } from '../resource-workspace/ResourceWorkspaceStore';
import styles from './appDialogs.module.scss';

export function ApplicationPicker({
  workspace,
  group,
  initial_url = '',
  on_close,
}: {
  workspace: ResourceWorkspaceStore;
  group: ResourceTabGroup;
  initial_url?: string;
  on_close: () => void;
}) {
  const client = workspace.client;
  const [url, setUrl] = useState(initial_url);
  const [apps, setApps] = useState<UiAppSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const request = new AbortController();
    setLoading(true);
    setError(null);
    if (!client) {
      setLoading(false);
      setError('Runtime 未连接');
      return () => request.abort();
    }
    void client
      .command({ type: 'list_ui_apps' }, { signal: request.signal })
      .then((result) => {
        if (!request.signal.aborted) {
          setApps(result.payload.apps);
          setError(
            result.payload.unavailable_sources.length
              ? `部分应用来源读取失败：${result.payload.unavailable_sources.join('、')}`
              : null,
          );
        }
      })
      .catch(() => {
        if (!request.signal.aborted) setError('应用列表读取失败，请重试。');
      })
      .finally(() => {
        if (!request.signal.aborted) setLoading(false);
      });
    return () => request.abort();
  }, [client, retry]);
  function open(target: UiAppSummary['target'] | { type: 'external'; url: string }) {
    try {
      workspace.openApp(target, {}, undefined, group);
      on_close();
    } catch (error) {
      setError(error instanceof Error ? error.message : '无法打开应用');
    }
  }
  return (
    <Dialog
      aria_label="打开应用"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      on_close={on_close}
    >
      <h2>打开应用</h2>
      {loading && <p role="status">正在读取应用…</p>}
      {error && (
        <p role="alert">
          {error}{' '}
          <Button size="small" variant="text" onClick={() => setRetry((value) => value + 1)}>
            重试
          </Button>
        </p>
      )}
      {!loading && !apps.length && !error && <p>暂无可用的内置应用</p>}
      <div className={styles.list}>
        {apps.map((app) => (
          <Button
            className={styles.entry}
            variant="text"
            key={JSON.stringify(app.target)}
            onClick={() => open(app.target)}
          >
            <span>{app.entry.title}</span>
            <span className={styles.origin}>
              {app.target.type === 'plugin' ? app.target.name : app.target.target.server_key}
            </span>
          </Button>
        ))}
      </div>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          open({ type: 'external', url });
        }}
      >
        <input
          className={styles.field}
          aria-label="外部应用地址"
          placeholder="请输入 HTTP 或 HTTPS 地址"
          value={url}
          onChange={(event) => setUrl(event.target.value)}
        />
        <div className={styles.actions}>
          <Button type="button" variant="text" onClick={on_close}>
            取消
          </Button>
          <Button type="submit" disabled={!url.trim()}>
            连接外部应用
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
