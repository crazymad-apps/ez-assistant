import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { WorkPlanSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { AnchoredOverlay } from "./AnchoredOverlay";
import styles from "./index.module.scss";

const TODO_EXIT_MOTION_MS = 110;

type TodoSummaryProps = Readonly<{
  on_open_change: (open: boolean) => void;
  open: boolean;
  running: boolean;
  work_plan: WorkPlanSnapshot;
}>;

export function TodoSummary(props: TodoSummaryProps) {
  const trigger_ref = useRef<HTMLButtonElement>(null);
  const overlay_ref = useRef<HTMLDivElement>(null);
  const [detail_mounted, setDetailMounted] = useState(props.open);
  const [message_mask, setMessageMask] = useState<Readonly<{
    host: HTMLElement;
    top: number;
  }> | null>(null);
  const current = props.work_plan.items.find((item) => item.status === "in_progress");
  const current_step = current
    ?? props.work_plan.items.find((item) => item.status === "pending");
  const current_step_text = current_step?.text ?? "工作计划待更新";

  useEffect(() => {
    if (props.open) {
      setDetailMounted(true);
      return;
    }
    const timeout = window.setTimeout(
      () => setDetailMounted(false),
      TODO_EXIT_MOTION_MS,
    );
    return () => window.clearTimeout(timeout);
  }, [props.open]);

  useLayoutEffect(() => {
    if (!detail_mounted) {
      setMessageMask(null);
      return;
    }

    let resize_observer: ResizeObserver | null = null;
    let animation_frame = 0;

    function updateMessageMask() {
      const trigger = trigger_ref.current;
      const overlay = overlay_ref.current;
      const host = trigger?.closest<HTMLElement>("[data-conversation-area]");
      if (!trigger || !overlay || !host) return;

      const host_rect = host.getBoundingClientRect();
      const trigger_rect = trigger.getBoundingClientRect();
      const overlay_height = overlay.getBoundingClientRect().height;
      const overlay_gap = 6;
      const viewport_padding = 8;
      const fade_buffer = 52;
      const overlay_top = Math.max(
        viewport_padding,
        trigger_rect.top - overlay_height - overlay_gap,
      );
      const top = Math.max(0, Math.floor(overlay_top - host_rect.top - fade_buffer));

      setMessageMask((previous) => (
        previous?.host === host && previous.top === top
          ? previous
          : { host, top }
      ));
    }

    // AnchoredOverlay 会在同一轮 layout effect 中完成首次定位；下一帧读取最终 DOM 尺寸。
    animation_frame = window.requestAnimationFrame(updateMessageMask);
    if (typeof ResizeObserver !== "undefined") {
      resize_observer = new ResizeObserver(updateMessageMask);
      if (overlay_ref.current) resize_observer.observe(overlay_ref.current);
      const host = trigger_ref.current?.closest<HTMLElement>("[data-conversation-area]");
      if (host) resize_observer.observe(host);
    }
    window.addEventListener("resize", updateMessageMask);
    document.addEventListener("scroll", updateMessageMask, true);
    return () => {
      window.cancelAnimationFrame(animation_frame);
      resize_observer?.disconnect();
      window.removeEventListener("resize", updateMessageMask);
      document.removeEventListener("scroll", updateMessageMask, true);
    };
  }, [detail_mounted, props.work_plan]);

  return (
    <>
      {message_mask && createPortal(
        <div
          aria-hidden="true"
          className={`${styles.todo_message_mask} ${props.open ? styles.todo_message_mask_open : ""}`}
          data-todo-message-mask
          style={{ top: message_mask.top }}
        />,
        message_mask.host,
      )}
      <div className={styles.todo_anchor}>
        <button
          aria-expanded={props.open}
          aria-haspopup="dialog"
          aria-label={`工作计划：${props.work_plan.objective}`}
          className={styles.todo_summary}
          onBlur={() => props.on_open_change(false)}
          onClick={() => props.on_open_change(true)}
          onFocus={() => props.on_open_change(true)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              props.on_open_change(false);
            }
          }}
          onMouseEnter={() => props.on_open_change(true)}
          onMouseLeave={() => props.on_open_change(false)}
          ref={trigger_ref}
          type="button"
        >
          {props.running && <span aria-hidden="true" className={styles.todo_loading} />}
          <strong title={current_step_text}>{current_step_text}</strong>
          <Icon name={props.open ? "chevron-up" : "chevron-down"} size={13} />
        </button>
        {detail_mounted && (
          <AnchoredOverlay
            aria_label="工作计划详情"
            class_name={`${styles.todo_detail} ${props.open ? styles.todo_detail_open : ""}`}
            horizontal_align="center"
            on_request_close={() => props.on_open_change(false)}
            overlay_ref={overlay_ref}
            placement="above"
            trigger_ref={trigger_ref}
          >
            <section
              aria-hidden={!props.open}
              aria-label="工作计划详情"
              data-todo-detail
              role="dialog"
            >
              <header>
                <strong title={props.work_plan.objective}>{props.work_plan.objective}</strong>
              </header>
              <div className={styles.todo_items}>
                {props.work_plan.items.length === 0 ? (
                  <p>当前计划没有 Todo 项。</p>
                ) : props.work_plan.items.map((item) => (
                  <div data-status={item.status} key={item.id}>
                    <i>{item.status === "completed" && <Icon name="check" size={12} />}</i>
                    <span title={item.text}>{item.text}</span>
                    <small>{todoStatusLabel(item.status)}</small>
                  </div>
                ))}
              </div>
            </section>
          </AnchoredOverlay>
        )}
      </div>
    </>
  );
}

function todoStatusLabel(status: WorkPlanSnapshot["items"][number]["status"]): string {
  if (status === "completed") return "已完成";
  if (status === "in_progress") return "进行中";
  return "待执行";
}
