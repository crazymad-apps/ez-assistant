import { useEffect, useState } from "react";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import { PdfViewer } from "../../../components/PdfViewer";
import type { SessionId } from "../../../generated/assistant-protocol";
import {
  NativeResourceFailure,
  previewAttachment,
  previewAttachmentSelection,
  type AttachmentPreview,
} from "../../../native-bridge/nativeResource";
import type { ComposerAttachment } from "./useComposerAttachments";
import styles from "./AttachmentDetailDialog.module.scss";

export function AttachmentDetailDialog(props: Readonly<{
  attachment: ComposerAttachment;
  on_close: () => void;
  session_id: SessionId | null;
}>) {
  const [preview, setPreview] = useState<AttachmentPreview | null>(null);
  const [preview_error, setPreviewError] = useState<string | null>(null);
  const [preview_fallback, setPreviewFallback] = useState<"unsupported" | "too_large" | null>(null);

  useEffect(() => {
    let active = true;
    setPreview(null);
    setPreviewError(null);
    setPreviewFallback(null);
    const request = props.attachment.attachment_id && props.session_id
      ? previewAttachment(props.session_id, props.attachment.attachment_id)
      : previewAttachmentSelection(props.attachment.selection_id);
    void request.then((value) => active && setPreview(value)).catch((reason: unknown) => {
      if (!active) return;
      if (reason instanceof NativeResourceFailure && reason.code === "resource_not_previewable") {
        setPreviewFallback("unsupported");
      } else if (reason instanceof NativeResourceFailure && reason.code === "resource_too_large") {
        setPreviewFallback("too_large");
      } else {
        setPreviewError(reason instanceof Error ? reason.message : "无法预览附件。");
      }
    });
    return () => { active = false; };
  }, [props.attachment.attachment_id, props.attachment.selection_id, props.session_id]);

  const media_type = preview?.media_type ?? props.attachment.media_type ?? "检测中";
  return (
    <Dialog
      aria_labelledby="composer-attachment-detail-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      on_close={props.on_close}
    >
      <header>
        <div>
          <span><Icon name="paperclip" size={17} /></span>
          <section>
            <h2 id="composer-attachment-detail-title">{props.attachment.original_name}</h2>
            <small>{formatBytes(props.attachment.size_bytes)}</small>
          </section>
        </div>
        <button aria-label="关闭附件详情" onClick={props.on_close} type="button"><Icon name="x" size={18} /></button>
      </header>
      <main>
        <dl>
          <div><dt>媒体类型</dt><dd>{media_type}</dd></div>
          <div><dt>来源</dt><dd>{props.attachment.origin === "clipboard" ? "剪贴板" : "文件选择器"}</dd></div>
          <div><dt>状态</dt><dd>{attachmentStatus(props.attachment)}</dd></div>
        </dl>
        <section className={styles.preview}>
          {!preview && !preview_error && !preview_fallback && <p className={styles.state}>正在读取附件预览…</p>}
          {preview_error && <p className={styles.error}>{preview_error}</p>}
          {preview_fallback && <p className={styles.state}>{preview_fallback === "too_large" ? "文件较大，无法在应用内预览。" : "此文件暂不支持应用内预览。"}</p>}
          {preview?.kind === "text" && <pre>{preview.text}</pre>}
          {preview?.kind === "image" && preview.data_url && <img alt={props.attachment.original_name} src={preview.data_url} />}
          {preview?.kind === "pdf" && preview.data_base64 && (
            <PdfViewer base64={preview.data_base64} title={`${props.attachment.original_name} PDF 预览`} />
          )}
        </section>
      </main>
      <footer><button onClick={props.on_close} type="button">关闭</button></footer>
    </Dialog>
  );
}

function attachmentStatus(attachment: ComposerAttachment): string {
  if (attachment.state === "selected") return attachment.origin === "clipboard" ? "将随消息发送" : "待发送";
  if (attachment.state === "uploading") return "上传中";
  if (attachment.state === "uploaded") return "已上传";
  return attachment.error ?? "上传失败，可重试";
}

function formatBytes(value: number): string {
  if (value >= 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  if (value >= 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${value} B`;
}
