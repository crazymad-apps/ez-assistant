import { useRootStore as useResourceRoot } from "../../../stores/RootStoreContext";
import {
  useEffect,
  useState,
  type KeyboardEvent,
  type MouseEvent,
} from "react";
import type {
  AttachmentSummary,
  ConversationInputSourceSnapshot,
  SessionSummary,
  QuotedTextSnapshot,
  ScheduleResultFile,
  UserMessageSnapshot,
} from "@ez-assistant/protocol";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import { PresenceBoundary } from "../../../components/Presence";
import { ConversationMarkdownContent } from "../../resource-workspace/ConversationMarkdownContent";
import {
  formatTime,
  runStatusLabel,
} from "./conversationRows";
import { DeviceMessageSourceDialog } from "./DeviceMessageSourceDialog";
import { QuoteDetailDialog } from "../QuoteDetailDialog";
import { MessageActions } from "./MessageActions";
import styles from "./index.module.scss";
import type { ResourceMenuLocation } from "../../resource-workspace/ResourceContextMenu";

export function UserMessage(props: Readonly<{
  message: UserMessageSnapshot;
  navigation_highlight?: boolean;
  attachments: readonly AttachmentSummary[];
  on_attachment_click: (attachment: AttachmentSummary) => void;
  on_attachment_menu?: (attachment: AttachmentSummary, location: ResourceMenuLocation) => void;
  source_session?: SessionSummary;
  on_source_open: (session: SessionSummary) => void;
  on_quote_locate?: (quote: QuotedTextSnapshot) => Promise<boolean>;
  editing?: boolean;
  edit?: Readonly<{ disabled_reason?: string; loading: boolean; on_select: () => void }>;
}>) {
  const images = props.attachments.filter((attachment) => attachment.media_type?.startsWith("image/"));
  const files = props.attachments.filter((attachment) => !attachment.media_type?.startsWith("image/"));
  const source = props.message.source ?? { type: "user" as const };
  const quotes = props.message.quotes ?? [];
  const is_external = source.type === "controller_delivery" || source.type === "proxy_report";
  const [device_source_open, setDeviceSourceOpen] = useState(false);
  const [quote_detail, setQuoteDetail] = useState<QuotedTextSnapshot | null>(null);
  if (source.type === "schedule_report") {
    return <ScheduledReport message={props.message} source={source} navigation_highlight={props.navigation_highlight} />;
  }
  return (
    <article
      className={`${styles.user_message}${is_external ? ` ${styles.external_message}` : ""}`}
      data-message-id={props.message.message_id}
      data-navigation-highlight={props.navigation_highlight || undefined}
      data-history-edit-target={props.editing || undefined}
      data-source={source.type}
    >
      {source.type === "controller_delivery" && (
        <div className={styles.message_source_header}>
          <strong>主控转达</strong>
          <button disabled={!props.source_session} onClick={() => props.source_session && props.on_source_open(props.source_session)} type="button">
            {props.source_session?.title ?? "主控会话不可用"}
          </button>
        </div>
      )}
      {source.type === "proxy_report" && (
        <div className={styles.message_source_header}>
          <strong>会话报告 · {runStatusLabel(source.source_run_status)}</strong>
          {source.source_goal_id && <span>关联目标</span>}
          <button disabled={!props.source_session} onClick={() => props.source_session && props.on_source_open(props.source_session)} type="button">
            {props.source_session ? `打开 ${props.source_session.title}` : "来源会话不可用"}
          </button>
        </div>
      )}
      {images.length > 0 && (
        <div aria-label="消息图片" className={styles.user_images}>
          {images.map((attachment) => (
            <MessageImage
              attachment={attachment}
              key={attachment.attachment_id}
              on_click={() => props.on_attachment_click(attachment)}
              on_menu={(location) => props.on_attachment_menu?.(attachment, location)}
            />
          ))}
        </div>
      )}
      {files.length > 0 && (
        <div className={styles.user_attachments}>
          {files.map((attachment) => (
            <button
              disabled={attachment.state !== "ready"}
              key={attachment.attachment_id}
              onClick={() => props.on_attachment_click(attachment)}
              onContextMenu={(event) => openAttachmentMenuFromPointer(event, attachment, props.on_attachment_menu)}
              onKeyDown={(event) => openAttachmentMenuFromKeyboard(event, attachment, props.on_attachment_menu)}
              type="button"
            >
              <Icon name="paperclip" size={13} />
              <span>{attachment.original_name}</span>
            </button>
          ))}
        </div>
      )}
      {quotes.length > 0 && (
        <div aria-label="消息引用" className={styles.user_quotes}>
          {quotes.map((quote) => (
            <button key={quote.quote_id} onClick={() => setQuoteDetail(quote)} type="button">
              <Icon name="quote" size={12} />
              <span>{quote.exact}</span>
            </button>
          ))}
        </div>
      )}
      {source.type === "proxy_report" ? (
        <ReportDisclosure title="查看原始汇报" text={props.message.text} quoteable />
      ) : (
        <div
          className={styles.user_bubble}
          data-quote-content="true"
          data-quote-role="user"
          data-quote-root="true"
        >{props.message.text}</div>
      )}
      {source.type === "device" && (
        <button
          aria-label={`查看消息来源：${source.device_name}`}
          className={styles.device_source_label}
          onClick={() => setDeviceSourceOpen(true)}
          type="button"
        >
          <Icon name="device" size={12} />
          <span>{source.device_name}</span>
        </button>
      )}
      {(props.message.skill || props.message.mcp_selection) && (
        <div className={styles.user_skill}>
          {props.message.skill && <span title={props.message.skill.name}>{props.message.skill.name}</span>}
          {props.message.mcp_selection && <span title={props.message.mcp_selection.server_key}>MCP · {props.message.mcp_selection.display_name}</span>}
        </div>
      )}
      <MessageActions copy_label="复制用户消息" text={props.message.text} time={formatTime(props.message.created_at_ms)}>
        {props.edit && (
          <Tooltip content={props.edit.disabled_reason ?? (props.edit.loading ? "正在读取原消息…" : "编辑用户消息")}>
            <button
              aria-busy={props.edit.loading || undefined}
              aria-disabled={Boolean(props.edit.disabled_reason || props.edit.loading) || undefined}
              aria-label="编辑用户消息"
              onClick={props.edit.disabled_reason || props.edit.loading ? undefined : props.edit.on_select}
              type="button"
            >
              <Icon name="edit" size={15} />
            </button>
          </Tooltip>
        )}
      </MessageActions>
      <PresenceBoundary present={source.type === "device" && device_source_open}>
      {source.type === "device" && device_source_open ? (
        <DeviceMessageSourceDialog on_close={() => setDeviceSourceOpen(false)} source={source} />
      ) : null}
      </PresenceBoundary>
      <PresenceBoundary present={quote_detail !== null}>
      {quote_detail && (
        <QuoteDetailDialog
          on_close={() => setQuoteDetail(null)}
          on_locate={() => props.on_quote_locate?.(quote_detail) ?? Promise.resolve(false)}
          quote={quote_detail}
        />
      )}
      </PresenceBoundary>
    </article>
  );
}

function ScheduledReport(props: Readonly<{
  message: UserMessageSnapshot;
  source: Extract<ConversationInputSourceSnapshot, { type: "schedule_report" }>;
  navigation_highlight?: boolean;
}>) {
  const { message, source } = props;
  const heading = message.text.split("\n", 1)[0];
  const is_heading = heading?.startsWith("定时任务「") && heading.includes("」 · ");
  const title = is_heading ? heading : "定时任务结果";
  const detail = is_heading ? message.text.slice(heading.length).trimStart() : message.text;
  return <article className={styles.schedule_report} data-message-id={message.message_id}
    data-navigation-highlight={props.navigation_highlight || undefined} data-source="schedule_report">
    <ReportDisclosure title={title} text={detail} />
    {source.files.length > 0 && <ScheduledReportFiles execution_id={source.execution_id} files={source.files} />}
  </article>;
}

function ScheduledReportFiles(props: Readonly<{ execution_id: string; files: readonly ScheduleResultFile[] }>) {
  const { files } = useResourceRoot();
  const [download_error, setDownloadError] = useState(false);
  const download = (file: ScheduleResultFile) => {
    setDownloadError(false);
    void files.downloadScheduleResultFile(props.execution_id, file.file_id, file.name).catch(() => setDownloadError(true));
  };
  return (
    <div className={styles.schedule_report_files}>
      {props.files.map((file) => (
        <button key={file.file_id} onClick={() => download(file)} title={file.name} type="button">
          <Icon name="paperclip" size={13} />
          <span>{file.title}</span>
        </button>
      ))}
      {props.files.length > 1 && <button type="button" onClick={() => {
        setDownloadError(false);
        void files.downloadScheduleResultBundle(props.execution_id, props.files.map((file) => file.file_id))
          .catch(() => setDownloadError(true));
      }}><Icon name="archive" size={13} /><span>下载全部文件</span></button>}
      {download_error && <span role="alert">文件下载失败。</span>}
    </div>
  );
}

function ReportDisclosure({ title, text, quoteable }: Readonly<{
  title: string; text: string; quoteable?: boolean;
}>) {
  // 仅在展开时渲染，避免折叠的历史汇报提前加载正文里的图片等资源。
  const [open, setOpen] = useState(false);
  return <details className={styles.report_disclosure} onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary title={title}>
      <Icon name="chevron-right" size={14} className={styles.report_chevron} />
      <Icon name={quoteable ? "message" : "clock"} size={15} />
      <span>{title}</span>
    </summary>
    {open && <div className={styles.report_detail} data-quote-content={quoteable || undefined}
      data-quote-role={quoteable ? "user" : undefined} data-quote-root={quoteable || undefined}>
      <ConversationMarkdownContent text={text} />
    </div>}
  </details>;
}

function MessageImage(props: Readonly<{
  attachment: AttachmentSummary;
  on_click: () => void;
  on_menu: (location: ResourceMenuLocation) => void;
}>) {

  const files = useResourceRoot().files;
  const [source, setSource] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void files.thumbnailAttachment(props.attachment.session_id, props.attachment.attachment_id)
      .then((value) => { if (active) setSource(value); })
      .catch(() => undefined);
    return () => { active = false; };
  }, [props.attachment.attachment_id, props.attachment.session_id]);
  return (
    <button
      aria-label={`预览图片 ${props.attachment.original_name}`}
      disabled={props.attachment.state !== "ready"}
      onClick={props.on_click}
      onContextMenu={(event) => openAttachmentMenuFromPointer(
        event,
        props.attachment,
        (_attachment, location) => props.on_menu(location),
      )}
      onKeyDown={(event) => openAttachmentMenuFromKeyboard(
        event,
        props.attachment,
        (_attachment, location) => props.on_menu(location),
      )}
      type="button"
    >
      {source ? <img alt={props.attachment.original_name} src={source} /> : <span><Icon name="paperclip" size={18} /></span>}
    </button>
  );
}

function openAttachmentMenuFromPointer(
  event: MouseEvent<HTMLElement>,
  attachment: AttachmentSummary,
  open: ((attachment: AttachmentSummary, location: ResourceMenuLocation) => void) | undefined,
) {
  if (!open) return;
  event.preventDefault();
  event.stopPropagation();
  event.currentTarget.focus();
  open(attachment, { x: event.clientX, y: event.clientY });
}

function openAttachmentMenuFromKeyboard(
  event: KeyboardEvent<HTMLElement>,
  attachment: AttachmentSummary,
  open: ((attachment: AttachmentSummary, location: ResourceMenuLocation) => void) | undefined,
) {
  if (!open || (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10"))) return;
  event.preventDefault();
  event.stopPropagation();
  const bounds = event.currentTarget.getBoundingClientRect();
  open(attachment, { x: bounds.left + 16, y: bounds.bottom });
}

export function EmptyConversation(props: Readonly<{ title: string; detail: string; on_retry?: () => void }>) {
  return <div className={styles.empty}><span><Icon name="message" size={21} /></span><strong>{props.title}</strong><p>{props.detail}</p>{props.on_retry && <button onClick={props.on_retry} type="button">重新读取</button>}</div>;
}
