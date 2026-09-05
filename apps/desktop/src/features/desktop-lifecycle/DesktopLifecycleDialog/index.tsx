import { observer } from "mobx-react-lite";
import { Dialog } from "../../../components/Dialog";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const DesktopLifecycleDialog = observer(function DesktopLifecycleDialog() {
  const lifecycle = useRootStore().desktop_lifecycle;
  const intent = lifecycle.intent;

  const stops_runtime = intent !== "quit_desktop" || lifecycle.stop_runtime_on_quit;
  const title = intent === "restart_runtime"
    ? "重启 Runtime？"
    : intent === "stop_runtime"
      ? "停止 Runtime？"
      : lifecycle.stop_runtime_on_quit
        ? "退出并停止 Runtime？"
        : "退出桌面客户端？";
  const confirm_label = intent === "restart_runtime"
    ? "重启 Runtime"
    : intent === "stop_runtime"
      ? "停止 Runtime"
      : lifecycle.stop_runtime_on_quit
        ? "退出并停止 Runtime"
        : "退出客户端";

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
              <p>退出桌面客户端后，Runtime 默认继续运行。</p>
              {lifecycle.terminal_count > 0 && <p>将关闭 {lifecycle.terminal_count} 个终端，终端内运行的进程将结束。</p>}
              <label className={styles.checkbox}>
                <input
                  checked={lifecycle.stop_runtime_on_quit}
                  disabled={lifecycle.pending}
                  onChange={(event) => lifecycle.setStopRuntimeOnQuit(event.currentTarget.checked)}
                  type="checkbox"
                />
                同时停止 Runtime
              </label>
            </>
          ) : (
            <p>{intent === "restart_runtime" ? "Runtime 将受控停止并启动新实例。" : "桌面客户端会保留，你可以稍后重新启动 Runtime。"}</p>
          )}
          {stops_runtime && (
            <div className={styles.impact}>
              <strong>本次操作影响</strong>
              <dl>
                <div><dt>活动 Run</dt><dd>{lifecycle.impact.active_runs}</dd></div>
                <div><dt>排队输入</dt><dd>{lifecycle.impact.queued_inputs}</dd></div>
                <div><dt>待审批</dt><dd>{lifecycle.impact.pending_approvals}</dd></div>
              </dl>
              {(lifecycle.impact.active_runs + lifecycle.impact.queued_inputs + lifecycle.impact.pending_approvals) > 0 && (
                <p>未完成的工作会被中断，请确认后继续。</p>
              )}
            </div>
          )}
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
