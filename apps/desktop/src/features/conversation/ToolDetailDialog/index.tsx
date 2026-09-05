import { useEffect, useState, type KeyboardEvent, type MouseEvent } from "react";
import type {
  RecallNavigationTarget,
  RecallToolDetailSnapshot,
  ToolDetailSnapshot,
  ToolFileReference,
  ToolInputSnapshot,
  TokenUsageSnapshot,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { PdfViewer } from "../../../components/PdfViewer";
import { Dialog } from "../../../components/Dialog";
import {
  NativeResourceFailure,
  openToolFileInSystem,
  previewToolFile,
  revealToolFileInDirectory,
  type AttachmentPreview,
} from "../../../native-bridge/nativeResource";
import styles from "./index.module.scss";
import { isPreviewableResource } from "../../resource-workspace/ResourceWorkspaceStore";
import {
  ResourceContextMenu,
  type ResourceMenuLocation,
} from "../../resource-workspace/ResourceContextMenu";

type ToolDetailDialogProps = Readonly<{
  detail: ToolDetailView | null;
  error: string | null;
  initial_file_ref_id?: string | null;
  is_loading: boolean;
  on_close: () => void;
  on_file_open?: (file: ToolFileReference) => void;
  on_recall_navigate?: (target: RecallNavigationTarget) => void;
}>;

export type ToolDetailView = Pick<
  ToolDetailSnapshot,
  | "tool_name"
  | "status"
  | "input"
  | "request_json"
  | "result_summary"
  | "result_json"
  | "recall"
  | "stdout"
  | "stderr"
  | "error"
  | "files"
  | "output_truncated"
  | "historical_fields_missing"
> & Partial<Pick<ToolDetailSnapshot, "owner" | "message_id" | "call_id" | "image_inspection" | "mcp_identity">>
  & Readonly<{ source?: "reliable" | "live" }>;

export function ToolDetailDialog({
  detail,
  error,
  initial_file_ref_id,
  is_loading,
  on_close,
  on_file_open,
  on_recall_navigate,
}: ToolDetailDialogProps) {
  const [selected_file, setSelectedFile] = useState<ToolFileReference | null>(null);
  const [file_preview, setFilePreview] = useState<AttachmentPreview | null>(null);
  const [file_error, setFileError] = useState<string | null>(null);
  const [file_preview_fallback, setFilePreviewFallback] = useState<"unsupported" | "too_large" | null>(null);
  const [file_loading, setFileLoading] = useState(false);
  const [unavailable_file_refs, setUnavailableFileRefs] = useState<ReadonlySet<string>>(new Set());
  const [file_menu, setFileMenu] = useState<Readonly<{
    file: ToolFileReference;
    location: ResourceMenuLocation;
  }> | null>(null);
  const owner = detail?.owner;
  const is_read_image = detail?.tool_name === "read_image";
  const mcp_identity = detail?.mcp_identity ?? (detail?.input.type === "mcp" ? detail.input.identity : null);

  useEffect(() => {
    setUnavailableFileRefs(new Set());
    if (!detail) {
      setSelectedFile(null);
      return;
    }
    const requested_file = initial_file_ref_id
      ? detail.files.find((file) => file.resource_ref_id === initial_file_ref_id)
      : null;
    // read_image 的唯一主要产物就是图片，打开详情时直接加载，不再要求用户先经过文件列表。
    const primary_image = detail.tool_name === "read_image"
      ? detail.files.find((file) => file.origin === "session_tool_image")
      : null;
    setSelectedFile(requested_file ?? primary_image ?? null);
  }, [detail, initial_file_ref_id]);

  useEffect(() => {
    if (!selected_file || !owner || !detail?.message_id || selected_file.state !== "available") {
      setFilePreview(null);
      return;
    }
    let active = true;
    setFilePreview(null);
    setFileError(null);
    setFilePreviewFallback(null);
    setFileLoading(true);
    void previewToolFile(owner, detail.message_id, selected_file.resource_ref_id)
      .then((value) => {
        if (!active) {
          return;
        }
        setFilePreview(value);
        setUnavailableFileRefs((current) => {
          const next = new Set(current);
          next.delete(selected_file.resource_ref_id);
          return next;
        });
      })
      .catch((reason: unknown) => {
        if (!active) {
          return;
        }
        if (reason instanceof NativeResourceFailure && reason.code === "resource_not_previewable") {
          setFilePreviewFallback("unsupported");
        } else if (reason instanceof NativeResourceFailure && reason.code === "resource_too_large") {
          setFilePreviewFallback("too_large");
        } else {
          setFileError(reason instanceof Error ? reason.message : "无法预览文件。");
          setUnavailableFileRefs((current) => new Set(current).add(selected_file.resource_ref_id));
        }
      })
      .finally(() => active && setFileLoading(false));
    return () => { active = false; };
  }, [detail?.message_id, owner, selected_file]);

  async function openSelectedFile() {
    if (!selected_file || !owner || !detail?.message_id) {
      return;
    }
    setFileError(null);
    try {
      await openToolFileInSystem(owner, detail.message_id, selected_file.resource_ref_id);
    } catch (reason: unknown) {
      setFileError(reason instanceof Error ? reason.message : "无法打开文件。");
    }
  }

  async function revealSelectedFile() {
    if (!selected_file || !owner || !detail?.message_id) {
      return;
    }
    setFileError(null);
    try {
      await revealToolFileInDirectory(owner, detail.message_id, selected_file.resource_ref_id);
    } catch (reason: unknown) {
      setFileError(reason instanceof Error ? reason.message : "无法在目录中显示文件。");
    }
  }

  async function copyDisplayPath() {
    if (!selected_file?.display_path) {
      return;
    }
    try {
      await navigator.clipboard.writeText(selected_file.display_path);
    } catch {
      setFileError("无法复制文件路径。");
    }
  }

  function showFileMenu(
    file: ToolFileReference,
    event: MouseEvent<HTMLElement> | KeyboardEvent<HTMLElement>,
  ) {
    if ("key" in event && event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    event.preventDefault();
    event.currentTarget.focus();
    const location = "clientX" in event
      ? { x: event.clientX, y: event.clientY }
      : (() => {
          const bounds = event.currentTarget.getBoundingClientRect();
          return { x: bounds.left + 16, y: bounds.bottom };
        })();
    setFileMenu({ file, location });
  }

  function runFileAction(request: Promise<void>, fallback: string) {
    setFileError(null);
    void request.catch((failure: unknown) => {
      setFileError(failure instanceof Error ? failure.message : fallback);
    });
  }

  return (
    <Dialog
      aria_labelledby="tool-detail-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={is_read_image ? styles.image_dialog : styles.dialog}
      on_close={on_close}
    >
        <header className={styles.header}>
          <div className={styles.title_group}>
            <span className={styles.tool_icon}><Icon name="terminal" size={17} /></span>
            <div>
              <h2 id="tool-detail-title">{mcp_identity ? `${mcp_identity.server_display_name} (${mcp_identity.server_key}) / ${mcp_identity.tool_name}` : detail?.tool_name ?? "工具详情"}</h2>
              {detail && <span>{statusLabel(detail.status)}</span>}
            </div>
          </div>
          <button aria-label="关闭工具详情" onClick={on_close} type="button"><Icon name="x" size={18} /></button>
        </header>
        <div className={styles.body}>
          {is_loading && <p className={styles.state}>正在读取工具详情…</p>}
          {error && <p className={styles.error}>{error}</p>}
          {detail && (
            is_read_image ? (
              <ReadImageDetail
                detail={detail}
                file={selected_file}
                file_error={file_error}
                file_loading={file_loading}
                file_preview={file_preview}
                file_preview_fallback={file_preview_fallback}
                on_open={on_file_open}
                on_open_menu={showFileMenu}
              />
            ) : <>
              <DetailSection title="请求参数">
                {detail.input.type !== "image_inspection" && detail.request_json
                  ? <JsonBlock text={detail.request_json} />
                  : <ToolInput input={detail.input} is_live={detail.source === "live"} />}
              </DetailSection>
              {mcp_identity && <p className={styles.notice}>MCP 工具注解由服务自报，未经验证，不能作为安全或只读保证。</p>}
              <DetailSection title="执行结果">
                {detail.image_inspection && (
                  <dl className={styles.facts}>
                    <div><dt>辅助模型</dt><dd>{detail.image_inspection.auxiliary_model}</dd></div>
                    <div><dt>耗时</dt><dd>{detail.image_inspection.elapsed_ms} ms</dd></div>
                    <div><dt>辅助用量</dt><dd>{formatUsage(detail.image_inspection.usage)}</dd></div>
                  </dl>
                )}
                {detail.recall
                  ? <RecallResult on_navigate={on_recall_navigate} recall={detail.recall} />
                  : detail.result_json
                    ? <JsonBlock text={detail.result_json} />
                    : detail.result_summary && <p>{detail.result_summary}</p>}
                {detail.stdout && <OutputBlock label="stdout" text={detail.stdout} />}
                {detail.stderr && <OutputBlock label="stderr" text={detail.stderr} />}
                {detail.error && <p className={styles.error}>{detail.error.message}</p>}
                {!detail.recall && !detail.result_json && !detail.result_summary
                  && !detail.stdout && !detail.stderr && !detail.error && (
                  <p className={styles.muted}>当前记录没有可展示的结果内容。</p>
                )}
              </DetailSection>
              {detail.files.length > 0 && (
                <DetailSection title="文件">
                  <ul className={styles.file_list}>
                    {detail.files.map((file) => (
                      <li key={file.resource_ref_id}>
                        <button
                          disabled={file.state !== "available" || unavailable_file_refs.has(file.resource_ref_id)}
                          onClick={() => {
                            if (on_file_open && isPreviewableResource(file.display_name, file.media_type)) {
                              on_file_open(file);
                            } else {
                              setSelectedFile(file);
                            }
                          }}
                          onContextMenu={(event) => showFileMenu(file, event)}
                          onKeyDown={(event) => showFileMenu(file, event)}
                          type="button"
                        >
                          <span>{file.display_name}</span>
                          <small>
                            {file.state === "available" && !unavailable_file_refs.has(file.resource_ref_id)
                              ? "预览"
                              : "不可用"}
                          </small>
                        </button>
                        {file.display_path && <code>{file.display_path}</code>}
                      </li>
                    ))}
                  </ul>
                </DetailSection>
              )}
              {selected_file && (
                <DetailSection title="文件预览">
                  <div className={styles.file_preview_header}>
                    <strong>{selected_file.display_name}</strong>
                    {selected_file.origin !== "session_tool_image" && (
                      <div>
                        {selected_file.display_path && (
                          <button onClick={() => void copyDisplayPath()} type="button">复制路径</button>
                        )}
                        <button onClick={() => void revealSelectedFile()} type="button">在目录中打开</button>
                        <button onClick={() => void openSelectedFile()} type="button">系统打开</button>
                      </div>
                    )}
                  </div>
                  {file_loading && <p className={styles.muted}>正在读取文件预览…</p>}
                  {file_error && <p className={styles.error}>{file_error}</p>}
                  {file_preview_fallback && (
                    <p className={styles.muted}>
                      {file_preview_fallback === "too_large"
                        ? "文件较大，无法在应用内预览。可以使用系统应用打开或在目录中查看。"
                        : "此文件暂不支持应用内预览。可以使用系统应用打开或在目录中查看。"}
                    </p>
                  )}
                  {file_preview?.kind === "text" && <pre className={styles.file_preview_text}>{file_preview.text}</pre>}
                  {file_preview?.kind === "image" && file_preview.data_url && (
                    <img alt={selected_file.display_name} className={styles.file_preview_image} src={file_preview.data_url} />
                  )}
                  {file_preview?.kind === "pdf" && file_preview.data_base64 && (
                    <PdfViewer base64={file_preview.data_base64} title={`${selected_file.display_name} PDF 预览`} />
                  )}
                </DetailSection>
              )}
              {(detail.output_truncated || detail.historical_fields_missing) && (
                <p className={styles.notice}>
                  {detail.output_truncated
                    ? "输出已按安全上限截断。"
                    : detail.source === "live"
                      ? "当前为实时详情，可靠记录同步后可查看完整内容。"
                      : "较早记录缺少部分详情字段。"}
                </p>
              )}
            </>
          )}
        </div>
        {file_menu && owner && detail?.message_id && (
          <ResourceContextMenu
            items={[
              {
                disabled: !on_file_open || !isPreviewableResource(
                  file_menu.file.display_name,
                  file_menu.file.media_type,
                ),
                label: "在资源栏打开",
                on_select: () => on_file_open?.(file_menu.file),
              },
              {
                disabled: file_menu.file.origin === "session_tool_image",
                label: "使用系统应用打开",
                on_select: () => runFileAction(
                  openToolFileInSystem(owner, detail.message_id!, file_menu.file.resource_ref_id),
                  "无法使用系统应用打开。",
                ),
              },
              {
                disabled: file_menu.file.origin === "session_tool_image",
                label: "在 Finder 中显示",
                on_select: () => runFileAction(
                  revealToolFileInDirectory(owner, detail.message_id!, file_menu.file.resource_ref_id),
                  "无法在 Finder 中显示。",
                ),
              },
            ]}
            location={file_menu.location}
            on_close={() => setFileMenu(null)}
          />
        )}
    </Dialog>
  );
}

function ReadImageDetail({
  detail,
  file,
  file_error,
  file_loading,
  file_preview,
  file_preview_fallback,
  on_open,
  on_open_menu,
}: Readonly<{
  detail: ToolDetailView;
  file: ToolFileReference | null;
  file_error: string | null;
  file_loading: boolean;
  file_preview: AttachmentPreview | null;
  file_preview_fallback: "unsupported" | "too_large" | null;
  on_open?: (file: ToolFileReference) => void;
  on_open_menu: (
    file: ToolFileReference,
    event: MouseEvent<HTMLElement> | KeyboardEvent<HTMLElement>,
  ) => void;
}>) {
  const source_path = detail.input.type === "file" ? detail.input.path : null;
  const unavailable = file?.state === "unavailable";
  return (
    <div className={styles.image_detail}>
      <div className={styles.image_preview_stage}>
        {file_loading && <p className={styles.muted}>正在读取图片…</p>}
        {file_preview?.kind === "image" && file_preview.data_url && (
          <img alt={source_path ?? file?.display_name ?? "工具读取的图片"} src={file_preview.data_url} />
        )}
        {file_error && <p className={styles.error}>{file_error}</p>}
        {file_preview_fallback && (
          <p className={styles.muted}>
            {file_preview_fallback === "too_large" ? "图片较大，无法在应用内预览。" : "此图片暂不支持应用内预览。"}
          </p>
        )}
        {!file_loading && !file_preview && !file_error && !file_preview_fallback && unavailable && (
          <p className={styles.muted}>图片已不可用。</p>
        )}
        {!file_loading && !file && detail.status !== "running" && !detail.error && (
          <p className={styles.muted}>本次工具调用没有可预览的图片。</p>
        )}
        {detail.status === "running" && !file && <p className={styles.muted}>正在等待图片结果…</p>}
        {detail.error && <p className={styles.error}>{detail.error.message}</p>}
      </div>
      <dl className={styles.image_metadata}>
        {source_path && <div><dt>路径</dt><dd title={source_path}>{source_path}</dd></div>}
        {file?.media_type && <div><dt>格式</dt><dd>{file.media_type}</dd></div>}
        {file?.size_bytes !== null && file?.size_bytes !== undefined && (
          <div><dt>大小</dt><dd>{formatBytes(file.size_bytes)}</dd></div>
        )}
      </dl>
      {file?.state === "available" && on_open && (
        <button
          onClick={() => on_open(file)}
          onContextMenu={(event) => on_open_menu(file, event)}
          onKeyDown={(event) => on_open_menu(file, event)}
          type="button"
        >在资源栏打开</button>
      )}
      {(detail.output_truncated || detail.historical_fields_missing) && (
        <p className={styles.notice}>
          {detail.output_truncated ? "工具记录已截断。" : "较早记录缺少部分附属信息。"}
        </p>
      )}
    </div>
  );
}

function DetailSection({ title, children }: Readonly<{ title: string; children: React.ReactNode }>) {
  return <section className={styles.section}><h3>{title}</h3>{children}</section>;
}

function ToolInput({ input, is_live }: Readonly<{ input: ToolInputSnapshot; is_live: boolean }>) {
  switch (input.type) {
    case "shell":
      return <>
        <code className={styles.command}>{input.command || "（空命令）"}</code>
        <dl className={styles.facts}>
          <div><dt>工作目录</dt><dd>{input.working_directory || "未记录"}</dd></div>
          <div><dt>超时</dt><dd>{input.timeout_ms ? `${input.timeout_ms} ms` : "未设置"}</dd></div>
          <div><dt>进程模式</dt><dd>{input.process_mode || "默认"}</dd></div>
        </dl>
      </>;
    case "file":
      return <dl className={styles.facts}>
        <div><dt>操作</dt><dd>{input.operation}</dd></div>
        <div><dt>路径</dt><dd>{input.path}</dd></div>
      </dl>;
    case "delegation":
      return <><strong>{input.title}</strong><p>{input.task_summary}</p></>;
    case "general":
      return <p>{input.summary}</p>;
    case "mcp":
      return <JsonBlock text={input.arguments_json} />;
    case "image_inspection":
      return <dl className={styles.facts}>
        <div><dt>识别目标</dt><dd>{input.goal}</dd></div>
        {input.background && <div><dt>补充背景</dt><dd>{input.background}</dd></div>}
        <div><dt>图片</dt><dd>{input.image_paths.join("、")}</dd></div>
      </dl>;
    case "unavailable":
      return <p className={styles.muted}>
        {is_live ? "执行期间暂未提供结构化输入。" : "较早记录没有可安全恢复的输入事实。"}
      </p>;
  }
}

function formatUsage(usage: TokenUsageSnapshot | null): string {
  if (!usage) {
    return "Provider 未返回";
  }
  return `${usage.input_tokens} 输入 / ${usage.output_tokens} 输出 / ${usage.total_tokens} 总计`;
}

function formatBytes(size_bytes: number): string {
  if (size_bytes < 1024) {
    return `${size_bytes} B`;
  }
  if (size_bytes < 1024 * 1024) {
    return `${(size_bytes / 1024).toFixed(1)} KB`;
  }
  return `${(size_bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function OutputBlock({ label, text }: Readonly<{ label: string; text: string }>) {
  return <div className={styles.output}><span>{label}</span><pre>{text}</pre></div>;
}

/** 请求参数与结果保留 JSON 结构，不再将对象压成难读的单行正文。 */
function JsonBlock({ text }: Readonly<{ text: string }>) {
  return <pre className={styles.json_block}>{text}</pre>;
}

function RecallResult({
  on_navigate,
  recall,
}: Readonly<{
  on_navigate?: (target: RecallNavigationTarget) => void;
  recall: RecallToolDetailSnapshot;
}>) {
  return (
    <div className={styles.recall_result}>
      {recall.items.length > 0 ? (
        <ol className={styles.recall_list}>
          {recall.items.map((item, index) => (
            <li key={`${item.navigation?.message_id ?? "unavailable"}-${index}`}>
              <div className={styles.recall_meta}>
                <span>{roleLabel(item.role)}</span>
                {item.created_at_ms !== null && <time>{formatRecallTime(item.created_at_ms)}</time>}
              </div>
              <p>{item.content}</p>
              {item.navigation && on_navigate && (
                <button onClick={() => on_navigate(item.navigation!)} type="button">
                  打开来源会话
                  <Icon name="chevron-right" size={14} />
                </button>
              )}
            </li>
          ))}
        </ol>
      ) : <p className={styles.muted}>本次检索没有返回可展示的消息。</p>}
      {recall.failures.length > 0 && (
        <ul className={styles.recall_failures}>
          {recall.failures.map((failure) => (
            <li key={`${failure.source_id}-${failure.kind}`}>{failure.message}</li>
          ))}
        </ul>
      )}
      {recall.truncated && <p className={styles.muted}>结果已达到本次检索上限。</p>}
    </div>
  );
}

function roleLabel(role: string | null): string {
  return { assistant: "助手消息", user: "用户消息", tool: "工具消息" }[role ?? ""] ?? "会话消息";
}

function formatRecallTime(timestamp_ms: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(timestamp_ms));
}

function statusLabel(status: ToolDetailSnapshot["status"]): string {
  return { proposed: "等待执行", running: "正在执行", completed: "已完成", failed: "执行失败" }[status];
}
