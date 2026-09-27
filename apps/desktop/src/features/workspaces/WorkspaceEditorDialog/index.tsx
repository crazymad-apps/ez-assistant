import { observer } from "mobx-react-lite";
import { useMemo, useState, type TransitionEventHandler } from "react";
import { Dialog } from "../../../components/Dialog";
import type { PresenceState } from "../../../components/Presence";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import type { RootStore } from "../../../stores/RootStore";
import styles from "./index.module.scss";

type DirectoryDraft = Readonly<{ path: string }>;

export const WorkspaceEditorDialog = observer(function WorkspaceEditorDialog(props: Readonly<{
  editor?: NonNullable<RootStore["workspace_editor"]>;
  on_exit_transition_end?: TransitionEventHandler<HTMLDivElement>;
  presence_state?: PresenceState;
}>) {
  const store = useRootStore();
  const editor = props.editor ?? store.workspace_editor;
  const workspace = editor?.mode === "edit"
    ? store.projection.application?.workspaces.find((item) => item.workspace_id === editor.workspace_id)
    : null;
  const initial_primary = editor?.mode === "create" ? editor.primary_directory : workspace?.user_directory;
  const initial_label = editor?.mode === "create"
    ? (editor.primary_directory ? basename(editor.primary_directory) : "")
    : workspace?.label;
  const initial_additional = workspace?.additional_directories ?? [];
  const [label, setLabel] = useState(initial_label ?? "");
  const [directories, setDirectories] = useState<DirectoryDraft[]>(() => (
    initial_primary
      ? [initial_primary, ...initial_additional].map((path) => ({ path }))
      : []
  ));
  const [error, setError] = useState<string | null>(null);
  const [confirm_close, setConfirmClose] = useState(false);

  const initial_signature = useMemo(
    () => JSON.stringify({ label: initial_label ?? "", directories: initial_primary ? [initial_primary, ...initial_additional] : [] }),
    [initial_additional, initial_label, initial_primary],
  );
  if (!editor) return null;

  const dirty = JSON.stringify({ label, directories: directories.map((item) => item.path) }) !== initial_signature;

  function requestClose() {
    if (store.pending_workspace_action) return;
    if (dirty) setConfirmClose(true);
    else store.closeWorkspaceEditor();
  }

  async function addDirectory() {
    const path = await store.chooseWorkspaceDirectory();
    if (!path || store.workspace_editor !== editor) return;
    if (directories.some((directory) => directory.path === path)) {
      setError("该目录已经在当前项目中。");
      return;
    }
    if (directories.length >= 16) {
      setError("一个项目最多包含 16 个目录。");
      return;
    }
    setError(null);
    if (directories.length === 0 && !label.trim()) setLabel(basename(path));
    setDirectories((current) => [...current, { path }]);
  }

  async function replaceDirectory(index: number) {
    const path = await store.chooseWorkspaceDirectory(directories[index].path);
    if (!path || store.workspace_editor !== editor) return;
    if (directories.some((directory, i) => i !== index && directory.path === path)) { setError("该目录已经在当前项目中。"); return; }
    setError(null); setDirectories((current) => current.map((entry, i) => i === index ? {path} : entry));
  }

  function makePrimary(index: number) {
    setDirectories((current) => [current[index], ...current.filter((_, item_index) => item_index !== index)]);
  }

  function move(index: number, offset: -1 | 1) {
    const target = index + offset;
    if (index === 0 || target < 1 || target >= directories.length) return;
    setDirectories((current) => {
      const next = [...current];
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  }

  async function save() {
    const normalized_label = label.trim();
    if (!normalized_label || [...normalized_label].length > 80 || [...normalized_label].some((char) => /\p{Cc}/u.test(char))) {
      setError("项目名称需为 1–80 个字符，且不能包含控制字符。");
      return;
    }
    setError(null);
    const saved = await store.saveWorkspaceEditor({
      label: normalized_label,
      primary_directory: directories[0]?.path ?? null,
      additional_directories: directories.slice(1).map((directory) => directory.path),
    });
    if (!saved && store.workspace_editor === editor) setError(store.interaction_error ?? "保存项目失败，请检查后重试。");
  }

  return (
    <>
      <Dialog
        aria_label={editor.mode === "create" ? "新建项目" : "编辑项目"}
        backdrop_class_name={styles.backdrop}
        dialog_class_name={styles.dialog}
        dismissible={!store.pending_workspace_action}
        on_close={requestClose}
        on_exit_transition_end={props.on_exit_transition_end}
        presence_state={props.presence_state}
      >
        <header>
          <h3>{editor.mode === "create" ? "新建项目" : "编辑项目"}</h3>
          <button aria-label="关闭项目编辑" disabled={store.pending_workspace_action} onClick={requestClose} type="button">
            <Icon name="x" size={16} />
          </button>
        </header>
        <div className={styles.body}>
          <label className={styles.label_field}>
            <span>项目名称</span>
            <input placeholder="请输入" autoFocus maxLength={80} onChange={(event) => setLabel(event.currentTarget.value)} value={label} />
            <small>用于会话列表和智能体识别，不要求唯一。</small>
          </label>
          <section className={styles.directories} aria-label="工作目录">
            <div className={styles.directory_heading}>
              <div><strong>工作目录</strong><small>相对路径和命令行默认使用主目录。</small></div>
              <button disabled={directories.length >= 16} onClick={() => void addDirectory()} type="button"><Icon name="plus" size={14} />添加目录</button>
            </div>
            <ol>
              {directories.map((directory, index) => (
                <li key={directory.path}>
                  <span className={styles.order_handle} aria-hidden="true">⋮⋮</span>
                  <Icon name="folder" size={16} />
                  <span className={styles.directory_text}><strong>{basename(directory.path)}</strong><small title={directory.path}>{directory.path}</small></span>
                  <button type="button" aria-label={`更换 ${basename(directory.path)}`} onClick={() => void replaceDirectory(index)}>更换</button>
                  {index === 0 ? <span className={styles.directory_actions}><em>主要</em><button aria-label={`移除 ${basename(directory.path)}`} onClick={() => setDirectories((current) => current.filter((_, item_index) => item_index !== index))} type="button"><Icon name="trash" size={14} /></button></span> : (
                    <span className={styles.directory_actions}>
                      <button aria-label={`上移 ${basename(directory.path)}`} disabled={index === 1} onClick={() => move(index, -1)} type="button"><Icon name="chevron-up" size={14} /></button>
                      <button aria-label={`下移 ${basename(directory.path)}`} disabled={index === directories.length - 1} onClick={() => move(index, 1)} type="button"><Icon name="chevron-down" size={14} /></button>
                      <button onClick={() => makePrimary(index)} type="button">设为主目录</button>
                      <button aria-label={`移除 ${basename(directory.path)}`} onClick={() => setDirectories((current) => current.filter((_, item_index) => item_index !== index))} type="button"><Icon name="trash" size={14} /></button>
                    </span>
                  )}
                </li>
              ))}
            </ol>
            {directories.length === 0 && <p className={styles.private_directory_empty}>仅使用私有目录。未选择工作目录时，任务文件保存在项目私有目录。</p>}
            {editor.mode === "edit" && directories[0]?.path !== initial_primary && <p className={styles.directory_change_notice}>已有会话继续使用原目录，新会话使用更新后的目录。</p>}
          </section>
          {error && <p className={styles.error} role="alert">{error}</p>}
        </div>
        <footer>
          <button disabled={store.pending_workspace_action} onClick={requestClose} type="button">取消</button>
          <button className={styles.primary} disabled={store.pending_workspace_action} onClick={() => void save()} type="button">
            {store.pending_workspace_action ? "保存中…" : editor.mode === "create" ? "创建" : "保存"}
          </button>
        </footer>
      </Dialog>
        <Dialog aria_label="放弃项目修改" backdrop_class_name={styles.backdrop} dialog_class_name={styles.confirm_dialog} on_close={() => setConfirmClose(false)} open={confirm_close}>
          <header><h3>放弃未保存的修改？</h3></header>
          <p>名称或目录的修改尚未保存。</p>
          <footer><button onClick={() => setConfirmClose(false)} type="button">继续编辑</button><button className={styles.danger} onClick={() => { setConfirmClose(false); store.closeWorkspaceEditor(); }} type="button">放弃修改</button></footer>
        </Dialog>
    </>
  );
});

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}
