import { isTauri } from "@tauri-apps/api/core";
import * as native from "../native-bridge/nativeResource";
import { RuntimeClientError, type RuntimeClient } from "./RuntimeClient";
import type {
  SessionResourceLocator,
  HostFileRequest,
  ListHostFilesResult,
  PreviewSessionResourceFileResult,
  SessionMaterializationManifest,
  SessionMaterializationResult,
  UploadAttachmentResult,
  ConversationOwner,
} from "../generated/assistant-protocol";

/** 每个 RootStore 自己拥有浏览器 File 与上传操作。切换/退出释放引用，迟到结果不能进入新目标。 */
export class ClientResources {
  readonly desktop = isTauri();
  readonly #selections = new Map<string, File>();
  readonly #operations = new Map<string, AbortController>();
  readonly #pickers = new Set<() => void>();
  readonly #downloads = new Map<string, number>();
  #disposed = false;
  constructor(
    private readonly current: () => RuntimeClient | null,
    readonly local_host: boolean,
  ) {}
  get native_host(): boolean {
    return this.desktop && this.local_host;
  }
  get address(): string {
    return this.current()?.address ?? "";
  }
  dispose(): void {
    this.#disposed = true;
    for (const operation of this.#operations.values()) operation.abort();
    for (const close of this.#pickers) close();
    for (const [url, timer] of this.#downloads) {
      window.clearTimeout(timer);
      URL.revokeObjectURL(url);
    }
    this.#downloads.clear();
    this.#operations.clear();
    this.#selections.clear();
    this.#pickers.clear();
  }
  async #request<T>(
    path: string,
    init: RequestInit,
    consume: (response: Response) => Promise<T>,
  ): Promise<T> {
    const client = this.current();
    if (!client || this.#disposed) throw new Error("Host 尚未连接。");
    let result: T;
    try {
      result = await client.resource(path, init, consume);
    } catch (error) {
      if (error instanceof RuntimeClientError)
        throw new native.NativeResourceFailure(error.message, error.code);
      throw error;
    }
    if (this.#disposed || client !== this.current())
      throw new Error("连接已变化，请重新操作。");
    return result;
  }
  json = <T>(path: string, body: unknown, signal?: AbortSignal): Promise<T> =>
    this.#request(
      path,
      {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        signal,
      },
      (r) => r.json() as Promise<T>,
    );
  listHostFiles = (
    path: string | null,
    include_hidden: boolean,
    signal?: AbortSignal,
  ) =>
    this.json<ListHostFilesResult>(
      "/host-files/list",
      { path, include_hidden },
      signal,
    );
  selectHostDirectory = async (path: string) =>
    (await this.json<HostFileRequest>("/host-files/select-directory", { path }))
      .path;
  previewHostFile = (path: string) =>
    this.json<PreviewSessionResourceFileResult>("/host-files/preview", {
      path,
    });
  listSessionResourceFiles: typeof native.listSessionResourceFiles = (
    id,
    request,
  ) =>
    this.desktop
      ? native.listSessionResourceFiles(id, request)
      : this.json(
          `/sessions/${encodeURIComponent(id)}/resource-files/list`,
          request,
        );
  previewSessionResourceFile: typeof native.previewSessionResourceFile = (
    id,
    request,
  ) =>
    this.desktop
      ? native.previewSessionResourceFile(id, request)
      : this.json(
          `/sessions/${encodeURIComponent(id)}/resource-files/preview`,
          request,
        );
  chooseAttachmentFiles = async (): Promise<
    readonly native.AttachmentSelection[]
  > => {
    if (this.desktop) return native.chooseAttachmentFiles();
    if (this.#disposed) return [];
    return new Promise((resolve, reject) => {
      const input = document.createElement("input");
      input.type = "file";
      input.multiple = true;
      input.hidden = true;
      let done = false;
      const close = () => finish([]);
      const finish = (files: readonly File[]) => {
        if (done) return;
        done = true;
        input.remove();
        this.#pickers.delete(close);
        try {
          if (this.#selections.size + files.length > 128)
            throw new Error("待发送文件过多，请先发送或移除附件。");
          if (files.length > 32 || files.some((file) => file.size > 1024 ** 3))
            throw new Error("最多选择 32 个文件，单文件不超过 1 GiB。");
          resolve(files.map((file) => this.#stage(file, "file_picker")));
        } catch (error) {
          reject(error);
        }
      };
      input.addEventListener(
        "change",
        () => finish(Array.from(input.files ?? [])),
        { once: true },
      );
      input.addEventListener("cancel", close, { once: true });
      this.#pickers.add(close);
      document.body.append(input);
      input.click();
    });
  };
  #stage(
    file: File,
    origin: "clipboard" | "file_picker",
  ): native.AttachmentSelection {
    if (this.#disposed) throw new Error("连接已关闭。");
    if (this.#selections.size >= 128)
      throw new Error("待发送文件过多，请先发送或移除附件。");
    const selection_id = `web-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    this.#selections.set(selection_id, file);
    return {
      selection_id,
      original_name: file.name,
      size_bytes: file.size,
      media_type: file.type,
      origin,
    };
  }
  stageClipboardImage: typeof native.stageClipboardImage = async (file) =>
    this.desktop
      ? native.stageClipboardImage(file)
      : this.#stage(file, "clipboard");
  releaseAttachmentSelection: typeof native.releaseAttachmentSelection = async (
    id,
  ) => {
    if (this.desktop) await native.releaseAttachmentSelection(id);
    else this.#selections.delete(id);
  };
  cancelResourceOperation: typeof native.cancelResourceOperation = async (
    id,
  ) => {
    if (this.desktop) await native.cancelResourceOperation(id);
    else this.#operations.get(id)?.abort();
  };
  #file(id: string): File {
    const file = this.#selections.get(id);
    if (!file) throw new Error("文件选择已失效，请重新选择。");
    return file;
  }
  async #upload<T>(
    path: string,
    body: FormData,
    operation_id: string,
  ): Promise<T> {
    if (this.#operations.has(operation_id))
      throw new Error("上传操作仍在进行。");
    const abort = new AbortController();
    this.#operations.set(operation_id, abort);
    try {
      return await this.#request(
        path,
        { method: "POST", body, signal: abort.signal },
        (r) => r.json() as Promise<T>,
      );
    } catch (error) {
      // 网络中断不等于未提交。上层首次物化沿用 materialization key 查询/重试，不能生成另一份会话。
      if (
        error instanceof SyntaxError ||
        error instanceof TypeError ||
        (error instanceof DOMException && error.name === "AbortError")
      )
        throw new native.NativeResourceFailure(
          "上传结果尚未确认，可重试确认；切勿重复创建会话。",
          path === "/session-materializations"
            ? "materialization_response_unknown"
            : "upload_response_unknown",
        );
      throw error;
    } finally {
      if (this.#operations.get(operation_id) === abort)
        this.#operations.delete(operation_id);
    }
  }
  uploadSelectedAttachment: typeof native.uploadSelectedAttachment = async (
    session,
    selection,
    operation,
  ) => {
    if (this.desktop)
      return native.uploadSelectedAttachment(session, selection, operation);
    const file = this.#file(selection);
    const form = new FormData();
    form.append("file", file, file.name);
    const result = await this.#upload<UploadAttachmentResult>(
      `/sessions/${encodeURIComponent(session)}/attachments`,
      form,
      operation,
    );
    this.#selections.delete(selection);
    return result;
  };
  materializeNewSession = async (
    manifest: SessionMaterializationManifest,
    operation: string,
  ): Promise<SessionMaterializationResult> => {
    if (this.desktop) return native.materializeNewSession(manifest, operation);
    const form = new FormData();
    form.append("manifest", JSON.stringify(manifest));
    for (const declared of manifest.attachments ?? []) {
      const file = this.#file(declared.selection_key);
      if (
        file.name !== declared.original_name ||
        file.size !== declared.size_bytes
      )
        throw new Error("附件选择与发送清单不一致。");
      form.append(declared.selection_key, file, file.name);
    }
    const result = await this.#upload<SessionMaterializationResult>(
      "/session-materializations",
      form,
      operation,
    );
    for (const item of manifest.attachments ?? [])
      this.#selections.delete(item.selection_key);
    return result;
  };
  previewAttachmentSelection: typeof native.previewAttachmentSelection = async (
    id,
  ) =>
    this.desktop
      ? native.previewAttachmentSelection(id)
      : previewBlob(this.#file(id));
  previewAttachment: typeof native.previewAttachment = async (session, id) =>
    this.desktop
      ? native.previewAttachment(session, id)
      : this.#preview(
          `/sessions/${encodeURIComponent(session)}/attachments/${encodeURIComponent(id)}/preview`,
        );
  previewToolFile: typeof native.previewToolFile = async (
    owner,
    message,
    resource,
  ) =>
    this.desktop
      ? native.previewToolFile(owner, message, resource)
      : this.#preview(`${toolPath(owner, message, resource)}/preview`);
  async #preview(path: string): Promise<native.AttachmentPreview> {
    return this.#request(path, {}, async (r) => previewBlob(await r.blob()));
  }
  thumbnailAttachment: typeof native.thumbnailAttachment = async (
    session,
    id,
  ) =>
    this.desktop
      ? native.thumbnailAttachment(session, id)
      : this.#request(
          `/sessions/${encodeURIComponent(session)}/attachments/${encodeURIComponent(id)}/thumbnail`,
          {},
          async (r) => dataUrl(await r.blob()),
        );
  exportSessionMarkdown: typeof native.exportSessionMarkdown = async (
    session,
    name,
  ) => {
    if (this.desktop) return native.exportSessionMarkdown(session, name);
    await this.download(
      `/sessions/${encodeURIComponent(session)}/export.md`,
      name,
    );
    return true;
  };
  downloadHostFile = (path: string) =>
    this.download(
      "/host-files/download",
      path.split("/").at(-1) || "download",
      { path },
    );
  downloadSessionFile = (
    session: string,
    locator: SessionResourceLocator,
    name: string,
  ) =>
    this.download(
      `/sessions/${encodeURIComponent(session)}/resource-files/download`,
      name,
      { locator },
    );
  downloadAttachment = (session: string, id: string, name: string) =>
    this.download(
      `/sessions/${encodeURIComponent(session)}/attachments/${encodeURIComponent(id)}/download`,
      name,
    );
  downloadToolFile = (
    owner: ConversationOwner,
    message: string,
    id: string,
    name: string,
  ) => this.download(`${toolPath(owner, message, id)}/download`, name);
  download = async (
    path: string,
    name: string,
    body?: unknown,
  ): Promise<void> => {
    if (this.desktop) {
      await native.downloadRuntimeResource(path, name, body);
      return;
    }
    const blob = await this.#request(
      path,
      body === undefined
        ? {}
        : {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
          },
      (r) => r.blob(),
    );
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = name;
    document.body.append(link);
    try {
      link.click();
    } finally {
      link.remove();
      // 浏览器已取得下载引用后释放；失败或切换时也会回收，不持久化正文或 URL。
      this.#downloads.set(
        url,
        window.setTimeout(() => {
          URL.revokeObjectURL(url);
          this.#downloads.delete(url);
        }, 1000),
      );
    }
  };
}
export function toolPath(
  owner: ConversationOwner,
  message: string,
  resource: string,
): string {
  const child =
    owner.type === "child_task"
      ? `/child-tasks/${encodeURIComponent(owner.child_task_id)}`
      : "";
  return `/sessions/${encodeURIComponent(owner.session_id)}${child}/messages/${encodeURIComponent(message)}/resources/${encodeURIComponent(resource)}`;
}
async function previewBlob(blob: Blob): Promise<native.AttachmentPreview> {
  const mime = blob.type.split(";")[0];
  const image = ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(
    mime,
  );
  const pdf = mime === "application/pdf";
  if (blob.size > (image || pdf ? 16 : 4) * 1024 ** 2)
    throw new native.NativeResourceFailure(
      "文件超过预览限制。",
      "resource_too_large",
    );
  if (image || pdf) {
    const url = await dataUrl(blob);
    return {
      kind: image ? "image" : "pdf",
      media_type: mime,
      size_bytes: blob.size,
      text: null,
      data_url: image ? url : null,
      data_base64: pdf ? url.slice(url.indexOf(",") + 1) : null,
    };
  }
  const bytes = await blob.arrayBuffer();
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    if (text.includes("\0")) throw new Error();
  } catch {
    throw new native.NativeResourceFailure(
      "此文件不支持文本预览。",
      "resource_not_previewable",
    );
  }
  return {
    kind: "text",
    media_type: mime || "text/plain",
    size_bytes: blob.size,
    text,
    data_url: null,
  };
}
function dataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error("文件读取失败。"));
    reader.readAsDataURL(blob);
  });
}
