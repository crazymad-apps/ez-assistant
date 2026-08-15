import { useEffect, useState } from "react";
import type { AttachmentId } from "../../../generated/assistant-protocol";
import {
  cancelResourceOperation,
  chooseAttachmentFiles,
  releaseAttachmentSelection,
  uploadSelectedAttachment,
} from "../../../native-bridge/nativeResource";

export type ComposerAttachment = Readonly<{
  selection_id: string;
  original_name: string;
  size_bytes: number;
  state: "selected" | "uploading" | "uploaded" | "failed";
  attachment_id: AttachmentId | null;
  error: string | null;
  operation_id: string | null;
}>;

export function useComposerAttachments(options: Readonly<{
  disabled: boolean;
  on_error: (message: string) => void;
  session_id: string | null;
}>) {
  const [attachments, setAttachments] = useState<readonly ComposerAttachment[]>([]);
  const [pending, setPending] = useState(false);

  useEffect(() => {
    setAttachments((current) => {
      for (const attachment of current) {
        if (attachment.state !== "uploaded") {
          void releaseAttachmentSelection(attachment.selection_id).catch(() => undefined);
        }
      }
      return [];
    });
  }, [options.session_id]);

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
    setPending(true);
    try {
      const selected = await chooseAttachmentFiles();
      setAttachments((current) => [
        ...current,
        ...selected.map((item) => ({
          ...item,
          state: "selected" as const,
          attachment_id: null,
          error: null,
          operation_id: null,
        })),
      ]);
    } catch (error: unknown) {
      options.on_error(error instanceof Error ? error.message : "无法选择附件。");
    } finally {
      setPending(false);
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
    setPending(true);
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
      setPending(false);
    }
  }

  return {
    attachments,
    choose,
    clear: () => setAttachments([]),
    pending,
    remove,
    uploadAll,
  } as const;
}

function createOperationId(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `resource-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
