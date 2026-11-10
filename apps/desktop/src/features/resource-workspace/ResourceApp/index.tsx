import { useEffect, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Button } from '../../../components/Button';
import { InlineIconButton } from '../../../components/InlineIconButton';
import { McpAppView } from '../../mcp-apps';
import { useAppLinks } from '../../mcp-apps/useAppLinks';
import { useRootStore } from '../../../stores/RootStoreContext';
import type { AppController } from '../AppController';
import type { ResourceTabGroup, ResourceWorkspaceStore } from '../ResourceWorkspaceStore';
import styles from './index.module.scss';

export const ResourceApp = observer(function ResourceApp({
  controller,
  workspace,
  group,
}: {
  controller: AppController;
  workspace: ResourceWorkspaceStore;
  group: ResourceTabGroup;
}) {
  const root = useRootStore();
  const [connection_state, setConnectionState] = useState('connecting');
  const revision = root.reading_projection.card_sources_changed;
  const stale = root.reading_projection.is_stale;
  const client = workspace.client;
  const scope = group.scope_key;
  const active_tab = group.tabs.find((tab) => tab.type === 'app' && workspace.apps.get(tab.appId) === controller);
  const show_tool_approval = workspace.active_group === group && active_tab?.type === 'app' &&
    group.active_tab_key === `app:${active_tab.appId}` && root.navigation.right_sidebar_open;
  useEffect(() => {
    if (!stale) void controller.load(false, scope.startsWith('session:')
      ? { type: 'session', session_id: scope.slice(8) }
      : { type: 'draft' });
    return () => controller.disconnect();
  }, [controller, client, revision, stale, scope]);
  const links = useAppLinks(workspace, group, controller.capabilities, controller.source);
  useEffect(() => {
    if (links) controller.attach(links);
  }, [links, controller]);
  let status = '未连接';
  if (controller.source && connection_state === 'connected') status = '已连接';
  else if (controller.source && connection_state !== 'error') status = '连接中';
  return (
    <div className={styles.app} aria-label={`应用：${controller.title}`}>
      <header className={styles.toolbar}>
        <span>{controller.title}</span>
        <small>{controller.origin}</small>
        <small aria-label="应用连接状态">{status}</small>
        <InlineIconButton icon="refresh" label="重新连接应用" onClick={() => void controller.load()} />
      </header>
      {controller.pending && <p role="status">正在读取应用…</p>}
      {(controller.error || stale) && <p role="alert">{controller.error ?? '连接恢复后可重新连接应用'}</p>}
      {controller.target.type === 'external' && !controller.source && !stale && (
        <div className={styles.approval}>
          <p>连接外部应用</p>
          <p>{controller.origin}</p>
          <p>连接后可请求打开应用、向关联页面传递消息和填写输入草稿；草稿由你发送。</p>
          <Button onClick={() => void controller.load(true)}>连接</Button>
          <Button
            variant="text"
            onClick={() => {
              const tab = group.tabs.find((tab) => tab.type === 'app' && workspace.apps.get(tab.appId) === controller);
              if (tab?.type === 'app') workspace.closeTab(`app:${tab.appId}`, group);
            }}
          >
            取消
          </Button>
        </div>
      )}
      {controller.source && links && (
        <div className={styles.surface}>
          <McpAppView
            on_state_change={(state) => setConnectionState(state.status)}
            source={controller.source}
            links={links}
            tool_page={controller.tool_page}
            tool_page_current={() => links.live && group.scope_key === scope}
            show_tool_approval={show_tool_approval}
            input={controller.arguments_value}
            context={{
              displayMode: 'inline',
              availableDisplayModes: ['inline'],
              platform: root.local_files.desktop ? 'desktop' : 'web',
            }}
          />
        </div>
      )}
    </div>
  );
});
