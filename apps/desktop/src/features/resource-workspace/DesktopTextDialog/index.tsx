import { useState } from "react";
import { observer } from "mobx-react-lite";
import { Dialog } from "../../../components/Dialog";
import { Button } from "../../../components/Button";
import type { DesktopController } from "../DesktopController";
import styles from "./index.module.scss";

export const DesktopTextDialog = observer(function DesktopTextDialog({ controller, on_close }: Readonly<{ controller: DesktopController; on_close: () => void }>) {
  const [text, setText] = useState("");
  const [composing, setComposing] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const [attempted, setAttempted] = useState(false);
  const bytes = new TextEncoder().encode(text).length;
  return <Dialog aria_label="向远程桌面输入文本" backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog} on_close={on_close} dismissible={!controller.text_pending}>
    <h3>输入文本</h3>
    <textarea className={styles.input} aria-label="发送的文本" placeholder="使用本机输入法编辑，完成后点击发送" rows={6} value={text} disabled={controller.text_pending}
      onCompositionStart={() => setComposing(true)} onCompositionEnd={() => setComposing(false)}
      onChange={(event) => { setText(event.target.value); setAttempted(false); setResult(null); }} />
    <div className={styles.hint}>{bytes} / 16384 字节</div>
    {result && <div role="status">{result}</div>}
    {controller.control !== "controlled" && <div role="status">控制已停止，请关闭此窗口后重新开启控制。</div>}
    <div className={styles.actions}>
      <Button variant="text" onClick={on_close} disabled={controller.text_pending}>关闭</Button>
      <Button variant="primary" disabled={composing || attempted || controller.text_pending || controller.control !== "controlled" || bytes === 0 || bytes > 16384 || text.includes("\0")} onClick={() => {
        setAttempted(true);
        void controller.sendText(text).then(() => { setText(""); setResult("文本已交给目标设备。"); }, () => setResult("发送结果未确认，请先查看目标画面。文本未自动重发。"));
      }}>{controller.text_pending ? "正在发送…" : "发送"}</Button>
    </div>
  </Dialog>;
});
