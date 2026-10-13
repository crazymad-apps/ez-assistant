import { useRootStore as useResourceRoot } from "../../../stores/RootStoreContext";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
} from "react";
import type {
  AttachmentSummary,
  SessionSummary,
  QuotedTextSnapshot,
  UserMessageSnapshot,
} from "@ez-assistant/protocol";
import { Icon } from "../../../components/Icon";
import { PresenceBoundary } from "../../../components/Presence";
import {
  formatTime,
  runStatusLabel,
} from "./conversationRows";
import { DeviceMessageSourceDialog } from "./DeviceMessageSourceDialog";
import { QuoteDetailDialog } from "../QuoteDetailDialog";
import styles from "./index.module.scss";
import type { ResourceMenuLocation } from "../../resource-workspace/ResourceContextMenu";

export function UserMessage(props: Readonly<{
  message: UserMessageSnapshot;
  attachments: readonly AttachmentSummary[];
  on_attachment_click: (attachment: AttachmentSummary) => void;
  on_attachment_menu?: (attachment: AttachmentSummary, location: ResourceMenuLocation) => void;
  source_session?: SessionSummary;
  on_source_open: (session: SessionSummary) => void;
  on_quote_locate?: (quote: QuotedTextSnapshot) => Promise<boolean>;
}>) {
  const images = props.attachments.filter((attachment) => attachment.media_type?.startsWith("image/"));
  const files = props.attachments.filter((attachment) => !attachment.media_type?.startsWith("image/"));
  const source = props.message.source ?? { type: "user" as const };
  const quotes = props.message.quotes ?? [];
  const is_external = source.type === "controller_delivery" || source.type === "proxy_report";
  const [device_source_open, setDeviceSourceOpen] = useState(false);
  const [quote_detail, setQuoteDetail] = useState<QuotedTextSnapshot | null>(null);
  return (
    <article
      className={`${styles.user_message}${is_external ? ` ${styles.external_message}` : ""}`}
      data-message-id={props.message.message_id}
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
        <ProxyReportBody text={props.message.text} />
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
      <time>{formatTime(props.message.created_at_ms)}</time>
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

function ProxyReportBody(props: Readonly<{ text: string }>) {
  const viewport_ref = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [is_overflowing, setIsOverflowing] = useState(false);

  useLayoutEffect(() => {
    if (expanded) {
      return;
    }
    const viewport = viewport_ref.current;
    if (!viewport) {
      return;
    }
    const measure = () => {
      setIsOverflowing(viewport.scrollHeight > viewport.clientHeight + 1);
    };
    measure();
    if (typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver(measure);
    observer.observe(viewport);
    return () => observer.disconnect();
  }, [expanded, props.text]);

  return (
    <div className={styles.proxy_report_body} data-expanded={expanded}>
      <div className={styles.proxy_report_viewport} ref={viewport_ref}>
        <div
          className={styles.user_bubble}
          data-quote-content="true"
          data-quote-role="user"
          data-quote-root="true"
        >{props.text}</div>
      </div>
      {is_overflowing && (expanded ? (
        <button
          aria-expanded="true"
          className={styles.proxy_report_collapse}
          onClick={() => setExpanded(false)}
          type="button"
        >
          收起
          <Icon name="chevron-up" size={14} />
        </button>
      ) : (
        <button
          aria-expanded="false"
          aria-label="展开完整会话报告"
          className={styles.proxy_report_expand}
          onClick={() => setExpanded(true)}
          type="button"
        >
          <span>展开</span>
          <Icon name="chevron-down" size={14} />
        </button>
      ))}
    </div>
  );
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
