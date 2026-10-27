import { observer } from "mobx-react-lite";
import { Dialog } from "../../../components/Dialog";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const DesktopLifecycleDialog = observer(function DesktopLifecycleDialog() {
  const lifecycle = useRootStore().desktop_lifecycle;
  const intent = lifecycle.intent;

  const stops_runtime = intent !== "quit_desktop" || lifecycle.stop_runtime_on_quit;
  const title = intent === "restart_runtime"
    ? "重启后台服务？"
    : intent === "stop_runtime"
      ? "停止后台服务？"
      : "退出应用？";
  const confirm_label = intent === "restart_runtime"
    ? "重启"
    : intent === "stop_runtime"
      ? "停止"
      : lifecycle.stop_runtime_on_quit
        ? "退出并停止"
        : "保留后台并退出";

  return (
    <Dialog
      aria_labelledby="desktop-lifecycle-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      dismissible={!lifecycle.pending}
      on_close={() => lifecycle.dismiss()}
      open={intent !== null}
    >
        <header>
          <h3 id="desktop-lifecycle-title">{title}</h3>
        </header>
        <div className={styles.body}>
          {intent === "quit_desktop" ? (
            <>
              <p>停止后台服务会中断本机所有运行中的会话。取消勾选可让会话继续运行。</p>
              {lifecycle.terminal_count > 0 && <p>将关闭 {lifecycle.terminal_count} 个终端，终端内运行的进程将结束。</p>}
              <label className={styles.checkbox}>
                <input
                  checked={lifecycle.stop_runtime_on_quit}
                  disabled={lifecycle.pending}
                  onChange={(event) => lifecycle.setStopRuntimeOnQuit(event.currentTarget.checked)}
                  type="checkbox"
                />
                同时停止后台服务
              </label>
            </>
          ) : (
            <p>{intent === "restart_runtime" ? "重启本机后台服务会中断所有运行中的会话。" : "停止本机后台服务后，所有运行中的会话都会中断。"}</p>
          )}
          {!lifecycle.local_impact_known && <p>此操作只影响本机，远程设备会继续运行。</p>}
          {lifecycle.error_message && <p className={styles.error} role="alert">{lifecycle.error_message}</p>}
        </div>
        <footer>
          <button disabled={lifecycle.pending} onClick={() => lifecycle.dismiss()} type="button">取消</button>
          <button
            className={stops_runtime ? styles.danger : styles.primary}
            disabled={lifecycle.pending}
            onClick={() => void lifecycle.confirm()}
            type="button"
          >
            {lifecycle.pending ? "处理中…" : confirm_label}
          </button>
        </footer>
    </Dialog>
  );
});
