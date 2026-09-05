import { useState } from "react";
import { Dialog } from "../../../components/Dialog";
import { Button } from "../../../components/Button";
import styles from "./CloseTerminalDialog.module.scss";

export function CloseTerminalDialog(props: Readonly<{ title: string; on_cancel: () => void; on_confirm: () => Promise<void> }>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return <Dialog aria_label={`关闭 ${props.title}`} backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog}
    dismissible={!busy} on_close={props.on_cancel}>
    <h2>关闭 {props.title}？</h2>
    <p>关闭终端将终止其中正在运行的 Shell 和前台任务。</p>
    {error && <p role="alert">{error}</p>}
    <footer>
      <Button disabled={busy} onClick={props.on_cancel}>取消</Button>
      <Button disabled={busy} variant="danger" onClick={() => {
        setBusy(true);
        setError(null);
        void props.on_confirm().catch((failure: unknown) => {
          setError(failure instanceof Error ? failure.message : "关闭失败，请重试。");
          setBusy(false);
        });
      }}>{busy ? "正在关闭…" : "关闭终端"}</Button>
    </footer>
  </Dialog>;
}
