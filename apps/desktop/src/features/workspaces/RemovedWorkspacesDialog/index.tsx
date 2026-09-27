import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import type { WorkspaceSummary } from "@ez-assistant/protocol";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const RemovedWorkspacesDialog = observer(function RemovedWorkspacesDialog({
  open,
  on_close,
}: Readonly<{ open: boolean; on_close: () => void }>) {
  const store = useRootStore();
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [load_error, setLoadError] = useState<string | null>(null);
  const [restore_error, setRestoreError] = useState<Readonly<{ workspace_id: string; message: string }> | null>(null);

  useEffect(() => {
    if (!open) return undefined;
    let active = true;
    setLoading(true);
    setWorkspaces([]);
    setLoadError(null);
    setRestoreError(null);
    void store.listRemovedWorkspaces().then((result) => {
      if (!active) return;
      if (result) setWorkspaces(result);
      else setLoadError(store.interaction_error ?? "读取已移除项目失败，请重试。");
      setLoading(false);
    });
    return () => { active = false; };
  }, [open, store]);

  async function reload() {
    setLoading(true);
    setLoadError(null);
    const result = await store.listRemovedWorkspaces();
    if (result) setWorkspaces(result);
    else setLoadError(store.interaction_error ?? "读取已移除项目失败，请重试。");
    setLoading(false);
  }

  async function restore(workspace_id: string) {
    setRestoreError(null);
    const success = await store.restoreWorkspace(workspace_id);
    if (!success) {
      setRestoreError({ workspace_id, message: store.interaction_error ?? "恢复失败，请重试。" });
      return;
    }
    setWorkspaces((current) => current.filter((workspace) => workspace.workspace_id !== workspace_id));
    on_close();
  }

  return (
    <Dialog
      aria_label="恢复已移除项目"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      dismissible={!store.pending_workspace_action}
      on_close={on_close}
      open={open}
    >
      <header>
        <h3>恢复项目</h3>
        <button aria-label="关闭恢复窗口" disabled={store.pending_workspace_action} onClick={on_close} type="button"><Icon name="x" size={16} /></button>
      </header>
      {load_error && <div className={styles.error} role="alert">{load_error}<button disabled={loading} onClick={() => void reload()} type="button">重试</button></div>}
      <div className={styles.list}>
        {loading ? <p>正在读取已移除项目…</p> : workspaces.length === 0 && !load_error ? <p>暂无已移除项目</p> : null}
        {!loading && workspaces.map((workspace) => (
          <div className={styles.row} key={workspace.workspace_id}>
            <div className={styles.details}>
              <strong>{workspace.label}</strong>
              <small title={workspace.user_directory ?? undefined}>{workspace.user_directory ?? "仅使用私有目录"}</small>
              <small title={workspace.workspace_id}>ID {workspace.workspace_id}</small>
              <small>移除于 {workspace.removed_at_ms ? new Date(workspace.removed_at_ms).toLocaleString() : "时间未知"}</small>
              {restore_error?.workspace_id === workspace.workspace_id && <small className={styles.restore_error} role="alert">{restore_error.message}</small>}
            </div>
            <button disabled={store.pending_workspace_action} onClick={() => void restore(workspace.workspace_id)} type="button">{store.pending_workspace_action ? "恢复中…" : "恢复"}</button>
          </div>
        ))}
      </div>
      <footer><button disabled={store.pending_workspace_action} onClick={on_close} type="button">关闭</button></footer>
    </Dialog>
  );
});
