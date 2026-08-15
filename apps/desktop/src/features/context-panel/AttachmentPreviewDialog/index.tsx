import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { AttachmentSummary } from "../../../generated/assistant-protocol";
import {
  NativeResourceFailure,
  openAttachmentInSystem,
  previewAttachment,
  revealAttachmentInDirectory,
  type AttachmentPreview,
} from "../../../native-bridge/nativeResource";
import { Icon } from "../../../components/Icon";
import styles from "./index.module.scss";

export function AttachmentPreviewDialog(props: Readonly<{
  attachment: AttachmentSummary;
  on_close: () => void;
}>) {
  const [preview, setPreview] = useState<AttachmentPreview | null>(null);
  const [preview_error, setPreviewError] = useState<string | null>(null);
  const [preview_fallback, setPreviewFallback] = useState<"unsupported" | "too_large" | null>(null);
  const [action_error, setActionError] = useState<string | null>(null);
  const [action, setAction] = useState<"open" | "reveal" | null>(null);

  useEffect(() => {
    let active = true;
    setPreview(null);
    setPreviewError(null);
    setPreviewFallback(null);
    void previewAttachment(props.attachment.session_id, props.attachment.attachment_id)
      .then((value) => active && setPreview(value))
      .catch((reason: unknown) => {
        if (!active) {
          return;
        }
        if (reason instanceof NativeResourceFailure && reason.code === "resource_not_previewable") {
          setPreviewFallback("unsupported");
        } else if (reason instanceof NativeResourceFailure && reason.code === "resource_too_large") {
          setPreviewFallback("too_large");
        } else {
          setPreviewError(reason instanceof Error ? reason.message : "无法预览附件。");
        }
      });
    return () => { active = false; };
  }, [props.attachment.attachment_id, props.attachment.session_id]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => event.key === "Escape" && props.on_close();
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [props.on_close]);

  async function openInSystem() {
    setAction("open");
    setActionError(null);
    try {
      await openAttachmentInSystem(props.attachment.session_id, props.attachment.attachment_id);
    } catch (reason: unknown) {
      setActionError(reason instanceof Error ? reason.message : "无法打开附件。");
    } finally {
      setAction(null);
    }
  }

  async function revealInDirectory() {
    setAction("reveal");
    setActionError(null);
    try {
      await revealAttachmentInDirectory(props.attachment.session_id, props.attachment.attachment_id);
    } catch (reason: unknown) {
      setActionError(reason instanceof Error ? reason.message : "无法在目录中显示附件。");
    } finally {
      setAction(null);
    }
  }

  return createPortal(
    <div className={styles.backdrop} onMouseDown={(event) => {
      if (event.target === event.currentTarget) {
        props.on_close();
      }
    }}>
      <section aria-labelledby="attachment-preview-title" aria-modal="true" className={styles.dialog} role="dialog">
        <header>
          <div>
            <span><Icon name="paperclip" size={17} /></span>
            <section>
              <h2 id="attachment-preview-title">{props.attachment.original_name}</h2>
              <small>{formatBytes(props.attachment.size_bytes)}</small>
            </section>
          </div>
          <button aria-label="关闭附件预览" onClick={props.on_close} type="button"><Icon name="x" size={18} /></button>
        </header>
        <main>
          {!preview && !preview_error && !preview_fallback && <p className={styles.state}>正在读取附件预览…</p>}
          {preview_error && <p className={styles.error}>{preview_error}</p>}
          {preview_fallback && (
            <section className={styles.preview_fallback}>
              <span><Icon name="paperclip" size={20} /></span>
              <h3>{preview_fallback === "too_large" ? "文件较大，无法在应用内预览" : "此文件暂不支持应用内预览"}</h3>
              <p>{preview_fallback === "too_large"
                ? "为避免占用过多内存，请使用系统应用打开或在目录中查看。"
                : "文件本身仍然可用，可以使用系统应用打开或在目录中查看。"}</p>
            </section>
          )}
          {preview?.kind === "text" && <pre>{preview.text}</pre>}
          {preview?.kind === "image" && preview.data_url && (
            <img alt={props.attachment.original_name} src={preview.data_url} />
          )}
          {action_error && <p className={styles.error}>{action_error}</p>}
        </main>
        <footer>
          <button disabled={action !== null} onClick={() => void revealInDirectory()} type="button">
            {action === "reveal" ? "正在定位…" : "在目录中打开"}
          </button>
          <button disabled={action !== null} onClick={() => void openInSystem()} type="button">
            {action === "open" ? "正在打开…" : "使用系统应用打开"}
          </button>
          <button onClick={props.on_close} type="button">关闭</button>
        </footer>
      </section>
    </div>,
    document.body,
  );
}

function formatBytes(value: number): string {
  if (value >= 1024 * 1024) {
    return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  }
  if (value >= 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${value} B`;
}
