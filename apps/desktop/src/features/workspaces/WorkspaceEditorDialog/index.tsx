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
  const initial_label = editor?.mode === "create" ? basename(editor.primary_directory) : workspace?.label;
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
    () => JSON.stringify({ label: initial_label ?? "", directories: [initial_primary, ...initial_additional] }),
    [initial_additional, initial_label, initial_primary],
  );
  if (!editor || !initial_primary) return null;

  const dirty = JSON.stringify({ label, directories: directories.map((item) => item.path) }) !== initial_signature;

  function requestClose() {
    if (store.pending_workspace_action) return;
    if (dirty) setConfirmClose(true);
    else store.closeWorkspaceEditor();
  }

  async function addDirectory() {
    const path = await store.chooseWorkspaceDirectory();
    if (!path) return;
    if (directories.some((directory) => directory.path === path)) {
      setError("该目录已经在当前工作空间中。");
      return;
    }
    if (directories.length >= 16) {
      setError("一个工作空间最多包含 16 个目录。");
      return;
    }
    setError(null);
    setDirectories((current) => [...current, { path }]);
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
      setError("工作空间名称需为 1–80 个字符，且不能包含控制字符。");
      return;
    }
    setError(null);
    await store.saveWorkspaceEditor({
      label: normalized_label,
      primary_directory: directories[0].path,
      additional_directories: directories.slice(1).map((directory) => directory.path),
    });
  }

  return (
    <>
      <Dialog
        aria_label={editor.mode === "create" ? "新建工作空间" : "编辑工作空间"}
        backdrop_class_name={styles.backdrop}
        dialog_class_name={styles.dialog}
        dismissible={!store.pending_workspace_action}
        on_close={requestClose}
        on_exit_transition_end={props.on_exit_transition_end}
        presence_state={props.presence_state}
      >
        <header>
          <h3>{editor.mode === "create" ? "新建工作空间" : "编辑工作空间"}</h3>
          <button aria-label="关闭工作空间编辑" disabled={store.pending_workspace_action} onClick={requestClose} type="button">
            <Icon name="x" size={16} />
          </button>
        </header>
        <div className={styles.body}>
          <label className={styles.label_field}>
            <span>工作空间名称</span>
            <input autoFocus maxLength={80} onChange={(event) => setLabel(event.currentTarget.value)} value={label} />
            <small>用于会话列表和 Agent 识别，不要求唯一。</small>
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
                  {index === 0 ? <em>主要</em> : (
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
          </section>
          {error && <p className={styles.error} role="alert">{error}</p>}
        </div>
        <footer>
          <button disabled={store.pending_workspace_action} onClick={requestClose} type="button">取消</button>
          <button className={styles.primary} disabled={store.pending_workspace_action} onClick={() => void save()} type="button">
            {store.pending_workspace_action ? "保存中…" : "保存"}
          </button>
        </footer>
      </Dialog>
        <Dialog aria_label="放弃工作空间修改" backdrop_class_name={styles.backdrop} dialog_class_name={styles.confirm_dialog} on_close={() => setConfirmClose(false)} open={confirm_close}>
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
