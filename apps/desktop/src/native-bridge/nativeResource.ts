import { invoke, isTauri } from "@tauri-apps/api/core";
import type {
  AttachmentId,
  MessageId,
  ResourceRefId,
  SessionId,
  UploadAttachmentResult,
} from "../generated/assistant-protocol";

export type AttachmentSelection = Readonly<{
  selection_id: string;
  original_name: string;
  size_bytes: number;
}>;

export type AttachmentPreview = Readonly<{
  kind: "text" | "image";
  media_type: string;
  size_bytes: number;
  text: string | null;
  data_url: string | null;
}>;

export class NativeResourceFailure extends Error {
  readonly code: string | null;

  constructor(message: string, code: string | null = null) {
    super(message);
    this.name = "NativeResourceFailure";
    this.code = code;
  }
}

export async function chooseAttachmentFiles(): Promise<readonly AttachmentSelection[]> {
  ensureDesktopBridge();
  return invoke<AttachmentSelection[]>("choose_attachment_files").catch(normalizeResourceFailure);
}

export async function releaseAttachmentSelection(selection_id: string): Promise<void> {
  ensureDesktopBridge();
  await invoke("release_attachment_selection", { selectionId: selection_id }).catch(normalizeResourceFailure);
}

export async function cancelResourceOperation(operation_id: string): Promise<void> {
  ensureDesktopBridge();
  await invoke("cancel_resource_operation", { operationId: operation_id }).catch(normalizeResourceFailure);
}

export async function uploadSelectedAttachment(
  session_id: SessionId,
  selection_id: string,
  operation_id: string,
): Promise<UploadAttachmentResult> {
  ensureDesktopBridge();
  return invoke<UploadAttachmentResult>("upload_selected_attachment", {
    sessionId: session_id,
    selectionId: selection_id,
    operationId: operation_id,
  }).catch(normalizeResourceFailure);
}

export async function previewAttachment(
  session_id: SessionId,
  attachment_id: AttachmentId,
): Promise<AttachmentPreview> {
  ensureDesktopBridge();
  return invoke<AttachmentPreview>("preview_attachment", {
    sessionId: session_id,
    attachmentId: attachment_id,
  }).catch(normalizeResourceFailure);
}

export async function openAttachmentInSystem(
  session_id: SessionId,
  attachment_id: AttachmentId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("open_attachment_in_system", {
    sessionId: session_id,
    attachmentId: attachment_id,
  }).catch(normalizeResourceFailure);
}

export async function revealAttachmentInDirectory(
  session_id: SessionId,
  attachment_id: AttachmentId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("reveal_attachment_in_directory", {
    sessionId: session_id,
    attachmentId: attachment_id,
  }).catch(normalizeResourceFailure);
}

export async function previewToolFile(
  session_id: SessionId,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<AttachmentPreview> {
  ensureDesktopBridge();
  return invoke<AttachmentPreview>("preview_tool_file", {
    sessionId: session_id,
    messageId: message_id,
    resourceRefId: resource_ref_id,
  }).catch(normalizeResourceFailure);
}

export async function openToolFileInSystem(
  session_id: SessionId,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("open_tool_file_in_system", {
    sessionId: session_id,
    messageId: message_id,
    resourceRefId: resource_ref_id,
  }).catch(normalizeResourceFailure);
}

export async function revealToolFileInDirectory(
  session_id: SessionId,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("reveal_tool_file_in_directory", {
    sessionId: session_id,
    messageId: message_id,
    resourceRefId: resource_ref_id,
  }).catch(normalizeResourceFailure);
}

export async function exportSessionMarkdown(
  session_id: SessionId,
  suggested_name: string,
): Promise<boolean> {
  ensureDesktopBridge();
  const result = await invoke<{ readonly saved: boolean }>("export_session_markdown", {
    sessionId: session_id,
    suggestedName: suggested_name,
  }).catch(normalizeResourceFailure);
  return result.saved;
}

function ensureDesktopBridge(): void {
  if (!isTauri()) {
    throw new Error("浏览器预览未连接本机资源桥接。请在桌面应用中使用此功能。");
  }
}

function normalizeResourceFailure(error: unknown): never {
  if (typeof error === "object" && error !== null) {
    const candidate = error as Record<string, unknown>;
    if (typeof candidate.message === "string") {
      throw new NativeResourceFailure(
        candidate.message,
        typeof candidate.code === "string" ? candidate.code : null,
      );
    }
  }
  throw new NativeResourceFailure("本机资源操作失败。");
}
