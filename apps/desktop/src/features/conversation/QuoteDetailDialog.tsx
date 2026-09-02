import { useState } from "react";
import { Dialog } from "../../components/Dialog";
import { Icon } from "../../components/Icon";
import type { QuotedTextSnapshot } from "../../generated/assistant-protocol";
import styles from "./QuoteDetailDialog.module.scss";

export function QuoteDetailDialog(props: Readonly<{
  quote: QuotedTextSnapshot;
  on_close: () => void;
  on_locate: () => Promise<boolean>;
}>) {
  const [source_available, setSourceAvailable] = useState(props.quote.source_available);
  const [locating, setLocating] = useState(false);
  const locate = async () => {
    setLocating(true);
    const located = await props.on_locate();
    setLocating(false);
    if (located) props.on_close();
    else setSourceAvailable(false);
  };
  return (
    <Dialog
      aria_labelledby="quote-detail-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      on_close={props.on_close}
    >
      <header>
        <div><Icon name="quote" size={17} /><h2 id="quote-detail-title">引用详情</h2></div>
        <button aria-label="关闭引用详情" onClick={props.on_close} type="button"><Icon name="x" size={18} /></button>
      </header>
      <main>
        <dl>
          <div><dt>来源</dt><dd>{props.quote.source_label}</dd></div>
          <div><dt>角色</dt><dd>{props.quote.source_role === "user" ? "用户" : "助手"}</dd></div>
        </dl>
        <blockquote>
          {props.quote.prefix && <span>{props.quote.prefix}</span>}
          <mark>{props.quote.exact}</mark>
          {props.quote.suffix && <span>{props.quote.suffix}</span>}
        </blockquote>
        {!source_available && <p>来源已不可用，仍保留发送时冻结的引用内容。</p>}
      </main>
      <footer>
        <button disabled={!source_available || locating} onClick={() => void locate()} type="button">
          {locating ? "正在定位…" : "定位原消息"}
        </button>
        <button onClick={props.on_close} type="button">关闭</button>
      </footer>
    </Dialog>
  );
}
