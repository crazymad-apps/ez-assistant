import { useEffect, useState } from "react";
import type { AttachmentId } from "../../../generated/assistant-protocol";
import type { ComposerAttachment } from "../../../stores/NewSessionDraftStore";
import {
  cancelResourceOperation,
  chooseAttachmentFiles,
  releaseAttachmentSelection,
  stageClipboardImage,
  type AttachmentSelection,
  uploadSelectedAttachment,
} from "../../../native-bridge/nativeResource";

export type { ComposerAttachment } from "../../../stores/NewSessionDraftStore";

const MAX_COMPOSER_ATTACHMENTS = 32;
const MAX_CLIPBOARD_IMAGE_BYTES = 32 * 1024 * 1024;

export function useComposerAttachments(options: Readonly<{
  attachments?: readonly ComposerAttachment[];
  disabled: boolean;
  on_attachments_change?: (attachments: readonly ComposerAttachment[]) => void;
  on_error: (message: string) => void;
  owner_key: string | null;
}>) {
  const [local_attachments, setLocalAttachments] = useState<readonly ComposerAttachment[]>([]);
  const [pending_action, setPendingAction] = useState<"choose" | "paste" | "upload" | null>(null);
  const pending = pending_action !== null;
  const attachments = options.attachments ?? local_attachments;

  function setAttachments(
    update: readonly ComposerAttachment[] | ((current: readonly ComposerAttachment[]) => readonly ComposerAttachment[]),
  ) {
    const next = typeof update === "function" ? update(attachments) : update;
    if (options.on_attachments_change) {
      options.on_attachments_change(next);
    } else {
      setLocalAttachments(next);
    }
  }

  useEffect(() => {
    if (options.on_attachments_change) return;
    setLocalAttachments((current) => {
      for (const attachment of current) {
        if (attachment.state !== "uploaded") {
          void releaseAttachmentSelection(attachment.selection_id).catch(() => undefined);
        }
      }
      return [];
    });
  }, [options.owner_key, options.on_attachments_change]);

  function update(
    selection_id: string,
    patch: Partial<Omit<ComposerAttachment, "selection_id" | "original_name" | "size_bytes">>,
  ) {
    setAttachments((current) => current.map((item) => item.selection_id === selection_id
      ? { ...item, ...patch }
      : item));
  }

  async function choose() {
    if (pending || options.disabled) {
      return;
    }
    setPendingAction("choose");
    try {
      const selected = await chooseAttachmentFiles();
      if (attachments.length + selected.length > MAX_COMPOSER_ATTACHMENTS) {
        await Promise.allSettled(selected.map((item) => releaseAttachmentSelection(item.selection_id)));
        options.on_error("每条消息最多添加 32 个附件。");
        return;
      }
      setAttachments((current) => [
        ...current,
        ...selected.map((item) => ({
          ...item,
          media_type: item.media_type ?? null,
          origin: item.origin ?? "file_picker",
          state: "selected" as const,
          attachment_id: null,
          error: null,
          operation_id: null,
        })),
      ]);
    } catch (error: unknown) {
      options.on_error(error instanceof Error ? error.message : "无法选择附件。");
    } finally {
      setPendingAction(null);
    }
  }

  async function pasteImages(files: readonly File[]): Promise<boolean> {
    if (pending || options.disabled || files.length === 0) {
      return false;
    }
    if (attachments.length + files.length > MAX_COMPOSER_ATTACHMENTS) {
      options.on_error("每条消息最多添加 32 个附件，本次图片未添加。");
      return false;
    }
    if (files.some((file) => file.size === 0 || file.size > MAX_CLIPBOARD_IMAGE_BYTES)) {
      options.on_error("单张剪贴板图片不能超过 32 MiB，本次图片未添加。");
      return false;
    }

    setPendingAction("paste");
    const staged: AttachmentSelection[] = [];
    try {
      for (const file of files) {
        staged.push(await stageClipboardImage(file));
      }
      setAttachments((current) => [
        ...current,
        ...staged.map((item) => ({
          selection_id: item.selection_id,
          original_name: item.original_name,
          size_bytes: item.size_bytes,
          media_type: item.media_type ?? null,
          origin: "clipboard" as const,
          state: "selected" as const,
          attachment_id: null,
          error: null,
          operation_id: null,
        })),
      ]);
      return true;
    } catch (error: unknown) {
      await Promise.allSettled(staged.map((item) => releaseAttachmentSelection(item.selection_id)));
      options.on_error(error instanceof Error ? error.message : "无法添加剪贴板图片。");
      return false;
    } finally {
      setPendingAction(null);
    }
  }

  function remove(attachment: ComposerAttachment) {
    if (attachment.state === "uploading") {
      if (attachment.operation_id) {
        void cancelResourceOperation(attachment.operation_id).catch(() => undefined);
      }
      return;
    }
    setAttachments((current) => current.filter((item) => item.selection_id !== attachment.selection_id));
    if (attachment.state !== "uploaded") {
      void releaseAttachmentSelection(attachment.selection_id).catch(() => undefined);
    }
  }

  async function uploadAll(session_id: string): Promise<readonly AttachmentId[] | null> {
    setPendingAction("upload");
    const attachment_ids: AttachmentId[] = [];
    try {
      for (const attachment of attachments) {
        if (attachment.attachment_id) {
          attachment_ids.push(attachment.attachment_id);
          continue;
        }
        const operation_id = createOperationId();
        update(attachment.selection_id, { state: "uploading", error: null, operation_id });
        try {
          const result = await uploadSelectedAttachment(session_id, attachment.selection_id, operation_id);
          attachment_ids.push(result.attachment.attachment_id);
          update(attachment.selection_id, {
            state: "uploaded",
            attachment_id: result.attachment.attachment_id,
            error: null,
            operation_id: null,
          });
        } catch (error: unknown) {
          update(attachment.selection_id, {
            state: "failed",
            error: error instanceof Error ? error.message : "附件上传失败。",
            operation_id: null,
          });
          return null;
        }
      }
      return attachment_ids;
    } finally {
      setPendingAction(null);
    }
  }

  return {
    attachments,
    choose,
    clear: () => setAttachments([]),
    pending,
    pasteImages,
    paste_pending: pending_action === "paste",
    remove,
    uploadAll,
  } as const;
}

function createOperationId(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `resource-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
