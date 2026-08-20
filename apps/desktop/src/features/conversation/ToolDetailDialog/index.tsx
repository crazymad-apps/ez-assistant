import { useEffect, useState } from "react";
import type {
  RecallNavigationTarget,
  RecallToolDetailSnapshot,
  ToolDetailSnapshot,
  ToolFileReference,
  ToolInputSnapshot,
  TokenUsageSnapshot,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { Dialog } from "../../../components/Dialog";
import {
  NativeResourceFailure,
  openToolFileInSystem,
  previewToolFile,
  revealToolFileInDirectory,
  type AttachmentPreview,
} from "../../../native-bridge/nativeResource";
import styles from "./index.module.scss";

type ToolDetailDialogProps = Readonly<{
  detail: ToolDetailView | null;
  error: string | null;
  initial_file_ref_id?: string | null;
  is_loading: boolean;
  on_close: () => void;
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
> & Partial<Pick<ToolDetailSnapshot, "owner" | "message_id" | "call_id" | "image_inspection">>
  & Readonly<{ source?: "reliable" | "live" }>;

export function ToolDetailDialog({
  detail,
  error,
  initial_file_ref_id,
  is_loading,
  on_close,
  on_recall_navigate,
}: ToolDetailDialogProps) {
  const [selected_file, setSelectedFile] = useState<ToolFileReference | null>(null);
  const [file_preview, setFilePreview] = useState<AttachmentPreview | null>(null);
  const [file_error, setFileError] = useState<string | null>(null);
  const [file_preview_fallback, setFilePreviewFallback] = useState<"unsupported" | "too_large" | null>(null);
  const [file_loading, setFileLoading] = useState(false);
  const [unavailable_file_refs, setUnavailableFileRefs] = useState<ReadonlySet<string>>(new Set());
  const owner = detail?.owner;

  useEffect(() => {
    setSelectedFile(null);
    setUnavailableFileRefs(new Set());
  }, [detail?.call_id, detail?.message_id]);

  useEffect(() => {
    if (!detail || !initial_file_ref_id) {
      return;
    }
    setSelectedFile(detail.files.find((file) => file.resource_ref_id === initial_file_ref_id) ?? null);
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

  return (
    <Dialog
      aria_labelledby="tool-detail-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      on_close={on_close}
    >
        <header className={styles.header}>
          <div className={styles.title_group}>
            <span className={styles.tool_icon}><Icon name="terminal" size={17} /></span>
            <div>
              <h2 id="tool-detail-title">{detail?.tool_name ?? "工具详情"}</h2>
              {detail && <span>{statusLabel(detail.status)}</span>}
            </div>
          </div>
          <button aria-label="关闭工具详情" onClick={on_close} type="button"><Icon name="x" size={18} /></button>
        </header>
        <div className={styles.body}>
          {is_loading && <p className={styles.state}>正在读取工具详情…</p>}
          {error && <p className={styles.error}>{error}</p>}
          {detail && (
            <>
              <DetailSection title="请求参数">
                {detail.input.type !== "image_inspection" && detail.request_json
                  ? <JsonBlock text={detail.request_json} />
                  : <ToolInput input={detail.input} is_live={detail.source === "live"} />}
              </DetailSection>
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
                          onClick={() => setSelectedFile(file)}
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
    </Dialog>
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
