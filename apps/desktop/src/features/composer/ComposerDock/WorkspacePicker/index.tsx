import { observer } from 'mobx-react-lite';
import type { WorkspaceSummary } from '@ez-assistant/protocol';
import { ContextPicker, type ContextPickerOption } from '../ContextPicker';

/** 工作空间选项仍由草稿 Store 持有；选择器只映射展示信息。 */
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
  const options: ContextPickerOption[] = props.workspaces.map((workspace) => {
    const directory_name = workspace.user_directory ? lastPathSegment(workspace.user_directory) : null;
    const duplicate = props.workspaces.some((other) => other.workspace_id !== workspace.workspace_id
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
  const selected_has_directory = props.workspaces.some((workspace) => workspace.workspace_id === props.selected_workspace_id
    && workspace.user_directory !== null);

  return <ContextPicker action_icon="plus" action_label="新建项目" dialog_label="选择新会话项目"
    disabled={props.disabled} icon="folder" label="项目" on_action={props.on_create}
    on_open_change={props.on_open_change} on_select={props.on_select} open={props.open} options={options}
    selected={selected ? { ...selected, detail: selected_has_directory ? selected.detail : undefined } : null}
    trigger_label="新会话项目" />;
});

function lastPathSegment(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '');
  return normalized.split(/[\\/]/).at(-1) || path;
}
