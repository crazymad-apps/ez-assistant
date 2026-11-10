import { runInAction } from 'mobx';
import { useEffect, useMemo } from 'react';
import type { PluginUiCapability } from '@ez-assistant/protocol';
import type { ResourceTabGroup, ResourceWorkspaceStore } from '../resource-workspace/ResourceWorkspaceStore';
import { useRootStore } from '../../stores/RootStoreContext';
import { AppLinkEndpoint } from './appLinks';

/** 闭包固定发起方资源组；后台页面不能改用用户后来选中的会话。 */
export function useAppLinks(
  workspace: ResourceWorkspaceStore,
  group: ResourceTabGroup | undefined,
  capabilities: readonly PluginUiCapability[],
  source: unknown,
): AppLinkEndpoint | undefined {
  const root = useRootStore();
  const capability_key = capabilities.join(',');
  const scope = group?.scope_key;
  const revision = root.reading_projection.card_sources_changed;
  const client = workspace.client;
  const endpoint = useMemo(() => {
    if (!group || !source) return undefined;
    return new AppLinkEndpoint(
      capability_key.split(',').filter(Boolean) as PluginUiCapability[],
      async (request) => {
        if (workspace.groups.get(group.scope_key) !== group) throw new Error('资源组已关闭');
        const app = workspace.openApp(request.target, request.arguments, request.instance_key, group);
        if (root.reading_resources === workspace && workspace.active_group === group)
          runInAction(() => {
            root.navigation.right_sidebar_open = true;
          });
        await app.locate(request.arguments);
        return app.endpoint();
      },
      (text, active, signal) => root.requestAppComposerDraft(workspace, group.scope_key, text, active, signal),
      () =>
        root.reading_resources === workspace &&
        workspace.client === client &&
        !root.reading_projection.is_stale &&
        root.reading_projection.card_sources_changed === revision &&
        !group.closing &&
        !workspace.shutting_down &&
        workspace.groups.get(group.scope_key) === group,
    );
  }, [workspace, group, scope, capability_key, source, root, revision, client]);
  useEffect(() => () => endpoint?.retire(), [endpoint]);
  return endpoint;
}
