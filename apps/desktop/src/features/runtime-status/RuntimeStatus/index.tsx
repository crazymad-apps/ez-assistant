import { observer } from "mobx-react-lite";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

const labels = {
  booting: "正在启动",
  starting_runtime: "启动运行时",
  connecting: "正在连接",
  connected: "运行时已连接",
  reconnecting: "正在重连",
  stopping_runtime: "正在停止运行时",
  restarting_runtime: "正在重启运行时",
  runtime_stopped: "运行时已停止",
  disconnected: "运行时已断开",
  component_mismatch: "组件不匹配",
} as const;

export const RuntimeStatus = observer(function RuntimeStatus() {
  const store = useRootStore();
  const state = store.connection.state;
  const is_loading = state === "booting"
    || state === "starting_runtime"
    || state === "connecting"
    || state === "reconnecting"
    || state === "stopping_runtime"
    || state === "restarting_runtime";

  return (
    <div className={styles.status_group} data-tauri-drag-region>
      <div
        className={styles.status_pill}
        data-state={state}
        data-tauri-drag-region
        title={store.connection.error_message ?? undefined}
      >
        <span className={is_loading ? styles.loading_ring : styles.status_dot} aria-hidden="true" />
        <span>{labels[state]}</span>
      </div>
      {(state === "disconnected" || state === "component_mismatch" || state === "runtime_stopped") && (
        <button className={styles.retry_button} onClick={() => store.retryConnection()} type="button">
          重新连接
        </button>
      )}
    </div>
  );
});
