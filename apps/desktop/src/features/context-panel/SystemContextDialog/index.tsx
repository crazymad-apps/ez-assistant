import { useRef, useState } from "react";
import type { SystemContextSnapshot } from "../../../generated/assistant-protocol";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import { MarkdownContent } from "../../../components/MarkdownContent";
import styles from "./index.module.scss";

type ViewMode = "preview" | "source";

export function SystemContextDialog(props: Readonly<{
  snapshot: SystemContextSnapshot;
  on_close: () => void;
}>) {
  const close_button_ref = useRef<HTMLButtonElement>(null);
  const [view_mode, setViewMode] = useState<ViewMode>("preview");
  return (
    <Dialog
      aria_labelledby="system-context-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      initial_focus_ref={close_button_ref}
      on_close={props.on_close}
    >
      <header>
        <div>
          <h2 id="system-context-title">系统上下文</h2>
          <p>当前会话创建时冻结的系统上下文原文，不会随全局配置变化。</p>
        </div>
        <div className={styles.header_actions}>
          <div aria-label="系统上下文显示方式" className={styles.view_toggle} role="group">
            <button
              aria-pressed={view_mode === "preview"}
              data-state={view_mode === "preview" ? "active" : "inactive"}
              onClick={() => setViewMode("preview")}
              type="button"
            >
              预览
            </button>
            <button
              aria-pressed={view_mode === "source"}
              data-state={view_mode === "source" ? "active" : "inactive"}
              onClick={() => setViewMode("source")}
              type="button"
            >
              原文
            </button>
          </div>
          <button
            aria-label="关闭系统上下文"
            className={styles.close_button}
            onClick={props.on_close}
            ref={close_button_ref}
            type="button"
          >
            <Icon name="x" size={17} />
          </button>
        </div>
      </header>
      <div className={styles.body}>
        {props.snapshot.parts.length > 0 ? props.snapshot.parts.map((part, index) => (
          view_mode === "preview"
            ? <div className={styles.preview_part} key={index}><MarkdownContent text={markdownPreviewText(part)} /></div>
            : <pre className={styles.source_part} key={index}>{part}</pre>
        )) : <p className={styles.empty}>当前会话没有持久化的系统上下文内容。</p>}
      </div>
    </Dialog>
  );
}

function markdownPreviewText(source: string): string {
  return source
    .split("\n")
    .map((line) => /^\s*<\/?[A-Za-z][^>]*>\s*$/.test(line) ? line.replace("<", "\\<") : line)
    .join("\n");
}
