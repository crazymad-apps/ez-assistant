import { invoke, isTauri } from "@tauri-apps/api/core";
import type {
  AttachmentId,
  ConversationOwner,
  MessageId,
  ListSessionResourceFilesRequest,
  ListSessionResourceFilesResult,
  PreviewSessionResourceFileRequest,
  PreviewSessionResourceFileResult,
  ResourceRefId,
  SessionId,
  SessionMaterializationManifest,
  SessionMaterializationResult,
  SessionResourceLocator,
  UploadAttachmentResult,
} from "../generated/assistant-protocol";

export type AttachmentSelection = Readonly<{
  selection_id: string;
  original_name: string;
  size_bytes: number;
  media_type?: string | null;
  origin?: "file_picker" | "clipboard";
}>;

export type AttachmentPreview = Readonly<{
  kind: "text" | "image" | "pdf";
  media_type: string;
  size_bytes: number;
  text: string | null;
  data_url: string | null;
  data_base64?: string | null;
}>;

export type RegisteredLocalResource = Readonly<{
  resource_key: string;
  display_name: string;
  path_segments: readonly string[];
}>;

export type LocalResourcePreview = Readonly<{
  kind: "text" | "image" | "pdf";
  media_type: string;
  size_bytes: number;
  text: string | null;
  data_base64: string | null;
}>;

export type LocalResourceSibling = Readonly<{
  display_name: string;
  kind: "directory" | "file";
  current: boolean;
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

export async function stageClipboardImage(file: File): Promise<AttachmentSelection> {
  ensureDesktopBridge();
  const bytes = new Uint8Array(await file.arrayBuffer());
  return invoke<AttachmentSelection>("stage_clipboard_image", bytes, {
    headers: {
      "x-ez-media-type": file.type,
      "x-ez-original-name": encodeURIComponent(file.name),
    },
  }).catch(normalizeResourceFailure);
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

export async function materializeNewSession(
  manifest: SessionMaterializationManifest,
  operation_id: string,
): Promise<SessionMaterializationResult> {
  ensureDesktopBridge();
  return invoke<SessionMaterializationResult>("materialize_new_session", {
    manifest,
    operationId: operation_id,
  }).catch(normalizeResourceFailure);
}

export async function listSessionResourceFiles(
  session_id: SessionId,
  request: ListSessionResourceFilesRequest,
): Promise<ListSessionResourceFilesResult> {
  ensureDesktopBridge();
  return invoke<ListSessionResourceFilesResult>("list_session_resource_files", {
    sessionId: session_id,
    request,
  }).catch(normalizeResourceFailure);
}

export async function previewSessionResourceFile(
  session_id: SessionId,
  request: PreviewSessionResourceFileRequest,
): Promise<PreviewSessionResourceFileResult> {
  ensureDesktopBridge();
  return invoke<PreviewSessionResourceFileResult>("preview_session_resource_file", {
    sessionId: session_id,
    request,
  }).catch(normalizeResourceFailure);
}

export async function openSessionResourceInSystem(
  session_id: SessionId,
  locator: SessionResourceLocator,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("open_session_resource_in_system", {
    sessionId: session_id,
    locator,
  }).catch(normalizeResourceFailure);
}

export async function copySessionResourcePath(
  session_id: SessionId,
  locator: SessionResourceLocator,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("copy_session_resource_path", {
    sessionId: session_id,
    locator,
  }).catch(normalizeResourceFailure);
}

export async function revealSessionResourceInDirectory(
  session_id: SessionId,
  locator: SessionResourceLocator,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("reveal_session_resource_in_directory", {
    sessionId: session_id,
    locator,
  }).catch(normalizeResourceFailure);
}

export async function registerLocalFileUri(file_uri: string): Promise<RegisteredLocalResource> {
  ensureDesktopBridge();
  return invoke<RegisteredLocalResource>("register_local_file_uri", { fileUri: file_uri })
    .catch(normalizeResourceFailure);
}

export async function registerRelativeLocalResource(
  resource_key: string,
  reference: string,
): Promise<RegisteredLocalResource> {
  ensureDesktopBridge();
  return invoke<RegisteredLocalResource>("register_relative_local_resource", {
    resourceKey: resource_key,
    reference,
  }).catch(normalizeResourceFailure);
}

export async function previewLocalResource(resource_key: string): Promise<LocalResourcePreview> {
  ensureDesktopBridge();
  return invoke<LocalResourcePreview>("preview_local_resource", { resourceKey: resource_key })
    .catch(normalizeResourceFailure);
}

export async function listLocalResourceSiblings(resource_key: string): Promise<readonly LocalResourceSibling[]> {
  ensureDesktopBridge();
  return invoke<LocalResourceSibling[]>("list_local_resource_siblings", { resourceKey: resource_key })
    .catch(normalizeResourceFailure);
}

export async function registerLocalResourceSibling(
  resource_key: string,
  display_name: string,
): Promise<RegisteredLocalResource> {
  ensureDesktopBridge();
  return invoke<RegisteredLocalResource>("register_local_resource_sibling", {
    resourceKey: resource_key,
    displayName: display_name,
  }).catch(normalizeResourceFailure);
}

export async function openLocalResourceInSystem(resource_key: string): Promise<void> {
  ensureDesktopBridge();
  await invoke("open_local_resource_in_system", { resourceKey: resource_key }).catch(normalizeResourceFailure);
}

export async function revealLocalResourceInDirectory(resource_key: string): Promise<void> {
  ensureDesktopBridge();
  await invoke("reveal_local_resource_in_directory", { resourceKey: resource_key }).catch(normalizeResourceFailure);
}

export async function copyLocalResourcePath(resource_key: string): Promise<void> {
  ensureDesktopBridge();
  await invoke("copy_local_resource_path", { resourceKey: resource_key }).catch(normalizeResourceFailure);
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

export async function previewAttachmentSelection(selection_id: string): Promise<AttachmentPreview> {
  ensureDesktopBridge();
  return invoke<AttachmentPreview>("preview_attachment_selection", {
    selectionId: selection_id,
  }).catch(normalizeResourceFailure);
}

export async function thumbnailAttachment(
  session_id: SessionId,
  attachment_id: AttachmentId,
): Promise<string> {
  ensureDesktopBridge();
  return invoke<string>("thumbnail_attachment", {
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

export async function copyAttachmentPath(
  session_id: SessionId,
  attachment_id: AttachmentId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("copy_attachment_path", {
    sessionId: session_id,
    attachmentId: attachment_id,
  }).catch(normalizeResourceFailure);
}

export async function previewToolFile(
  owner: ConversationOwner,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<AttachmentPreview> {
  ensureDesktopBridge();
  return invoke<AttachmentPreview>("preview_tool_file", {
    sessionId: owner.session_id,
    childTaskId: owner.type === "child_task" ? owner.child_task_id : null,
    messageId: message_id,
    resourceRefId: resource_ref_id,
  }).catch(normalizeResourceFailure);
}

export async function openToolFileInSystem(
  owner: ConversationOwner,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("open_tool_file_in_system", {
    sessionId: owner.session_id,
    childTaskId: owner.type === "child_task" ? owner.child_task_id : null,
    messageId: message_id,
    resourceRefId: resource_ref_id,
  }).catch(normalizeResourceFailure);
}

export async function revealToolFileInDirectory(
  owner: ConversationOwner,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("reveal_tool_file_in_directory", {
    sessionId: owner.session_id,
    childTaskId: owner.type === "child_task" ? owner.child_task_id : null,
    messageId: message_id,
    resourceRefId: resource_ref_id,
  }).catch(normalizeResourceFailure);
}

export async function copyToolFilePath(
  owner: ConversationOwner,
  message_id: MessageId,
  resource_ref_id: ResourceRefId,
): Promise<void> {
  ensureDesktopBridge();
  await invoke("copy_tool_file_path", {
    sessionId: owner.session_id,
    childTaskId: owner.type === "child_task" ? owner.child_task_id : null,
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
