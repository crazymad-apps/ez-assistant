import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import { MarkdownContent } from "../../../components/MarkdownContent";
import type { DesktopUpdateStore } from "../DesktopUpdateStore";
import styles from "./index.module.scss";

export const DesktopUpdatePanel = observer(function DesktopUpdatePanel({ store }: Readonly<{ store: DesktopUpdateStore }>) {
  const state = store.snapshot;
  const error = store.command_error ?? state.error;
  const dismiss_label = state.required ? "关闭" : "稍后";
  const percent = state.total > 0 ? Math.min(100, Math.floor(state.received / state.total * 100)) : 0;
  return <Dialog open={store.open} on_close={() => store.hide()} dismissible={!store.installing}
    aria_labelledby="desktop-update-title" backdrop_class_name={styles.backdrop} dialog_class_name={styles.panel}>
    <h2 id="desktop-update-title">{store.installing ? "正在更新" : "有新版本可用"}</h2>
    <div className={styles.content}>
      {state.notes && <MarkdownContent text={state.notes} />}
      {store.installing ? <p role="status">正在完成更新，应用将自动重启…</p>
        : <p className={styles.notice}>{state.stops_local_host ? "更新将重启应用，并中断本机正在运行的任务。请先保存手头的工作。" : "更新将重启应用。请先保存手头的工作。"}</p>}
      {store.downloading && <div className={styles.progress} role="status">
        <span>正在下载更新 · {percent}%</span><progress aria-label="更新下载进度" max={state.total || 1} value={state.received} />
      </div>}
      {error && <p className={styles.error} role="alert">{error}</p>}
      {!state.available && !store.downloading && !store.installing && !error && <p>暂时无法更新，请稍后重试。</p>}
    </div>
    <footer>
      {!store.installing && <Button onClick={() => store.hide()}>{store.downloading ? "收起" : dismiss_label}</Button>}
      {store.downloading ? <Button onClick={() => void store.cancel()}>取消下载</Button>
        : !store.installing && state.available && <Button variant="primary" onClick={() => void store.start()}>{state.phase === "failed" ? "重试更新" : "更新并重启"}</Button>}
    </footer>
  </Dialog>;
});
