import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type {
  AssistantMessageSnapshot,
  AttachmentSummary,
  ChildTaskTreeItemSnapshot,
  MessageId,
  SessionSummary,
  ToolCallId,
  UserMessageSnapshot,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { thumbnailAttachment } from "../../../native-bridge/nativeResource";
import type { LiveRunProjection, LiveToolSnapshot } from "../../../stores/LiveExecutionStore";
import { ChildTaskTree } from "./ChildTaskTree";
import {
  collapsedSummary,
  formatDateTime,
  formatTime,
  runStatusLabel,
  runFailureMessage,
  segmentStateKey,
} from "./conversationRows";
import { AssistantSegmentView, LiveSteps } from "./ToolSegments";
import styles from "./index.module.scss";

export function UserMessage(props: Readonly<{
  message: UserMessageSnapshot;
  attachments: readonly AttachmentSummary[];
  on_attachment_click: (attachment: AttachmentSummary) => void;
  source_session?: SessionSummary;
  on_source_open: (session: SessionSummary) => void;
}>) {
  const images = props.attachments.filter((attachment) => attachment.media_type?.startsWith("image/"));
  const files = props.attachments.filter((attachment) => !attachment.media_type?.startsWith("image/"));
  const source = props.message.source ?? { type: "user" as const };
  const is_external = source.type !== "user";
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
            <MessageImage attachment={attachment} key={attachment.attachment_id} on_click={() => props.on_attachment_click(attachment)} />
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
              type="button"
            >
              <Icon name="paperclip" size={13} />
              <span>{attachment.original_name}</span>
            </button>
          ))}
        </div>
      )}
      {source.type === "proxy_report" ? (
        <ProxyReportBody text={props.message.text} />
      ) : (
        <div className={styles.user_bubble}>{props.message.text}</div>
      )}
      {props.message.skill && (
        <div className={styles.user_skill}>
          <span title={props.message.skill.name}>{props.message.skill.name}</span>
        </div>
      )}
      <time>{formatTime(props.message.created_at_ms)}</time>
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
        <div className={styles.user_bubble}>{props.text}</div>
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

function MessageImage(props: Readonly<{ attachment: AttachmentSummary; on_click: () => void }>) {
  const [source, setSource] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void thumbnailAttachment(props.attachment.session_id, props.attachment.attachment_id)
      .then((value) => { if (active) setSource(value); })
      .catch(() => undefined);
    return () => { active = false; };
  }, [props.attachment.attachment_id, props.attachment.session_id]);
  return (
    <button aria-label={`预览图片 ${props.attachment.original_name}`} disabled={props.attachment.state !== "ready"} onClick={props.on_click} type="button">
      {source ? <img alt={props.attachment.original_name} src={source} /> : <span><Icon name="paperclip" size={18} /></span>}
    </button>
  );
}

export function AssistantTurn(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  messages: readonly AssistantMessageSnapshot[];
  live_run: LiveRunProjection | null;
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  onFork: (message_id: MessageId) => void;
  show_fork: boolean;
  onLiveToolClick: (tool: LiveToolSnapshot) => void;
  onToolClick: (message_id: MessageId, call_id: ToolCallId) => void;
}>) {
  const last_message = props.messages.at(-1)!;
  const [collapsed, setCollapsed] = useState(false);
  const [reasoning_open, setReasoningOpen] = useState<Record<string, boolean>>({});
  const body_text = useMemo(
    () => props.messages.flatMap((message) => message.segments)
      .filter((segment) => segment.type === "text")
      .map((segment) => segment.text)
      .join("\n\n"),
    [props.messages],
  );
  const all_segments = useMemo(() => props.messages.flatMap((message) => message.segments), [props.messages]);
  const represented_tool_calls = useMemo(() => new Set([
    ...props.messages.flatMap((message) => message.segments)
      .flatMap((segment) => segment.type === "tool_group" ? segment.tools.map((tool) => tool.call_id) : []),
    ...(props.live_run?.steps.flatMap((step) => step.segments)
      .flatMap((segment) => segment.type === "tool_group" ? segment.tools.map((tool) => tool.call_id) : []) ?? []),
  ]), [props.live_run, props.messages]);
  const unattached_child_tasks = props.child_tasks.filter((item) => !represented_tool_calls.has(item.task.parent_tool_call_id));

  return (
    <article className={`${styles.assistant_message}${props.live_run ? ` ${styles.live_message}` : ""}`} data-message-id={last_message.message_id}>
      {collapsed ? (
        <button className={styles.collapsed_message} onClick={() => setCollapsed(false)} type="button">
          <span>消息已收起</span>
          <span>{collapsedSummary(all_segments)}</span>
          <Icon name="chevron-down" size={15} />
        </button>
      ) : (
        <div className={styles.assistant_segments}>
          {props.messages.map((message) => (
            <div className={styles.assistant_step} data-message-id={message.message_id} key={message.message_id}>
              {message.segments.map((segment, index) => {
                const state_key = segmentStateKey(message.message_id, segment, index);
                return (
                  <AssistantSegmentView
                    child_tasks={props.child_tasks}
                    key={state_key}
                    is_reasoning_open={segment.type === "reasoning" ? (reasoning_open[state_key] ?? false) : false}
                    message_id={message.message_id}
                    on_reasoning_toggle={() => segment.type === "reasoning" && setReasoningOpen((current) => ({ ...current, [state_key]: !current[state_key] }))}
                    on_child_open={props.on_child_open}
                    on_tool_click={props.onToolClick}
                    segment={segment}
                  />
                );
              })}
            </div>
          ))}
          {props.live_run && (
            <LiveSteps child_tasks={props.child_tasks} on_child_open={props.on_child_open} onToolClick={props.onLiveToolClick} run={props.live_run} />
          )}
          {props.live_run?.status === "failed" && <RunFailure run={props.live_run} />}
          {props.on_child_open && <ChildTaskTree embedded items={unattached_child_tasks} on_open={props.on_child_open} />}
        </div>
      )}
      {props.live_run?.status === "failed" ? null : props.live_run ? (
        <div className={styles.live_status}><span className={styles.live_dot} />{runStatusLabel(props.live_run.status)}</div>
      ) : (
        <MessageActions
          can_fork={last_message.can_fork}
          collapsed={collapsed}
          on_copy={() => void navigator.clipboard?.writeText(body_text)}
          on_fork={() => props.onFork(last_message.message_id)}
          on_toggle={() => setCollapsed((current) => !current)}
          show_fork={props.show_fork}
          time={formatDateTime(last_message.finished_at_ms)}
        />
      )}
    </article>
  );
}

export function LiveAssistantMessage(props: Readonly<{
  child_tasks?: readonly ChildTaskTreeItemSnapshot[];
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  onToolClick: (tool: LiveToolSnapshot) => void;
  run: LiveRunProjection;
}>) {
  return (
    <article className={`${styles.assistant_message} ${styles.live_message}`}>
      <div className={styles.assistant_segments}>
        <LiveSteps child_tasks={props.child_tasks ?? []} on_child_open={props.on_child_open} onToolClick={props.onToolClick} run={props.run} />
        {props.run.status === "failed" && <RunFailure run={props.run} />}
      </div>
      {props.run.status !== "failed" && <div className={styles.live_status}><span className={styles.live_dot} />{runStatusLabel(props.run.status)}</div>}
    </article>
  );
}

function RunFailure(props: Readonly<{ run: LiveRunProjection }>) {
  return (
    <p className={styles.run_error} role="alert">
      <span aria-hidden="true" />
      {runFailureMessage(props.run.model_failure_kind, props.run.error_code)}
    </p>
  );
}

export function EmptyConversation(props: Readonly<{ title: string; detail: string }>) {
  return <div className={styles.empty}><span><Icon name="message" size={21} /></span><strong>{props.title}</strong><p>{props.detail}</p></div>;
}

function MessageActions(props: Readonly<{
  can_fork: boolean;
  collapsed: boolean;
  on_copy: () => void;
  on_fork: () => void;
  on_toggle: () => void;
  show_fork: boolean;
  time: string;
}>) {
  return <div className={styles.message_actions}>
    <button aria-label="复制助手正文" onClick={props.on_copy} type="button"><Icon name="copy" size={15} /></button>
    {props.show_fork && <button aria-label="从此消息分叉" disabled={!props.can_fork} onClick={props.on_fork} type="button"><Icon name="fork" size={15} /></button>}
    <button aria-label={props.collapsed ? "展开消息" : "收起消息"} onClick={props.on_toggle} type="button">
      <Icon name={props.collapsed ? "chevron-down" : "chevron-up"} size={15} />
    </button>
    <time>{props.time}</time>
  </div>;
}
