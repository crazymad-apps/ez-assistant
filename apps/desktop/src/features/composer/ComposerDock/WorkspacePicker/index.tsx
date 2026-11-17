import { useEffect, useState } from 'react';
import { useRootStore } from '../../../../stores/RootStoreContext';
import { observer } from 'mobx-react-lite';
import type { WorkspaceSummary } from '@ez-assistant/protocol';
import { ContextPicker, type ContextPickerOption } from '../ContextPicker';

/** 打开时独立读取完整项目选项，页外项目不会注入侧栏。 */
export const WorkspacePicker = observer(function WorkspacePicker(props: Readonly<{
  disabled: boolean;
  has_draft: (workspace_id: string) => boolean;
  on_create: () => void;
  on_open_change: (open: boolean) => void;
  on_select: (workspace_id: string | null) => void;
  open: boolean;
  selected_workspace_id: string | null;
  workspaces: readonly WorkspaceSummary[];
}>) {
  const store = useRootStore();
  const target = store.navigation.target_device;
  const client = store.runtime_client;
  const [loaded, setLoaded] = useState<readonly WorkspaceSummary[] | null>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    let current = true;
    setLoaded(null);
    setError(false);
    if (props.open) void store.listWorkspaceOptions(target).then(rows => {
      if (current) setLoaded(rows);
    }).catch(() => { if (current) setError(true); });
    return () => { current = false; };
  }, [props.open, store, target, client]);
  const selected_workspace = store.projection.workspaceSummary(props.selected_workspace_id);
  const initial = selected_workspace && !props.workspaces.some(item => item.workspace_id === selected_workspace.workspace_id)
    ? [...props.workspaces, selected_workspace] : props.workspaces;
  const workspaces = loaded ?? initial;
  const options: ContextPickerOption[] = workspaces.map((workspace) => {
    const directory_name = workspace.user_directory ? lastPathSegment(workspace.user_directory) : null;
    const duplicate = workspaces.some((other) => other.workspace_id !== workspace.workspace_id
      && other.label === workspace.label
      && (other.user_directory ? lastPathSegment(other.user_directory) : null) === directory_name);
    const hints: string[] = [];
    if (duplicate) hints.push(`· ${workspace.workspace_id.slice(-6)}`);
    if (props.has_draft(workspace.workspace_id) && workspace.workspace_id !== props.selected_workspace_id) hints.push('有草稿');
    let detail: string | undefined;
    if (!directory_name) detail = '仅私有目录';
    else if (directory_name !== workspace.label) detail = directory_name;
    return { value: workspace.workspace_id, label: workspace.label, detail, hints,
      search_text: `${workspace.label} ${directory_name ?? ''}` };
  });
  const selected = options.find((option) => option.value === props.selected_workspace_id);
  const selected_has_directory = workspaces.some((workspace) => workspace.workspace_id === props.selected_workspace_id
    && workspace.user_directory !== null);

  return <ContextPicker notice={error ? "项目加载失败，请重新打开重试" : undefined} empty_text={props.open && !loaded && !error ? "正在加载项目…" : undefined} action_icon="plus" action_label="新建项目" dialog_label="选择新会话项目"
    disabled={props.disabled} icon="folder" label="项目" on_action={props.on_create}
    on_open_change={props.on_open_change} on_select={props.on_select} open={props.open} options={options}
    selected={selected ? { ...selected, detail: selected_has_directory ? selected.detail : undefined } : null}
    trigger_label="新会话项目" />;
});

function lastPathSegment(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '');
  return normalized.split(/[\\/]/).at(-1) || path;
}
