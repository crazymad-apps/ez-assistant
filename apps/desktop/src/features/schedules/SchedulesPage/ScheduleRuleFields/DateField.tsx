import { lazy, Suspense, useCallback, useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { AnchoredOverlay } from "../../../../components/AnchoredOverlay";
import { Icon } from "../../../../components/Icon";
import form_styles from "../index.module.scss";
import styles from "./index.module.scss";

const DayCalendar = lazy(() => import("./DayCalendar"));

export function ScheduleDateField(props: Readonly<{ value: string; disabled: boolean; on_change: (date: string) => void }>) {
  const [calendar, setCalendar] = useState({ open: false, generation: 0 });
  const trigger = useRef<HTMLButtonElement>(null);
  const overlay = useRef<HTMLDivElement>(null);
  const tab_frame = useRef(0);
  const label_id = useId();
  const value_id = useId();
  const panel_id = useId();
  const expanded = calendar.open && !props.disabled;
  const close = useCallback(() => setCalendar((current) => current.open ? { ...current, open: false } : current), []);
  useEffect(() => () => cancelAnimationFrame(tab_frame.current), [expanded]);

  function closeAndRestoreFocus() {
    close();
    trigger.current?.focus({ preventScroll: true });
  }

  function handleKey(event: KeyboardEvent) {
    if (!expanded) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeAndRestoreFocus();
    } else if (event.key === "Tab") {
      // WKWebView 用鼠标点击翻月时会把焦点交给浮层外的容器。只在 Tab 完成后观察焦点是否离开。
      cancelAnimationFrame(tab_frame.current);
      tab_frame.current = requestAnimationFrame(() => {
        const focused = document.activeElement;
        if (!overlay.current?.contains(focused) && !trigger.current?.contains(focused)) close();
      });
    }
  }

  return <div className={form_styles.field}>
    <span id={label_id}>日期</span>
    <button className={styles.date_trigger} aria-haspopup="dialog" aria-expanded={expanded}
      aria-labelledby={`${label_id} ${value_id}`} aria-controls={expanded ? panel_id : undefined}
      disabled={props.disabled} onClick={() => {
        if (expanded) close();
        else setCalendar((current) => ({ open: true, generation: current.generation + 1 }));
      }} onKeyDown={handleKey} ref={trigger} type="button">
      <span id={value_id} data-empty={!props.value || undefined}>{props.value || "选择日期"}</span>
      <Icon name="calendar" size={16} />
    </button>
    <AnchoredOverlay aria_label="选择日期" class_name={styles.date_popover} open={expanded}
      trigger_ref={trigger} overlay_ref={overlay} on_request_close={close}
      region_props={{ role: "dialog", onKeyDown: handleKey }}>
      <div id={panel_id}>
        <Suspense fallback={<p className={styles.calendar_loading} role="status">正在打开日历…</p>}>
          {/* 浮层退场时会保留正文。快速重新打开也要从已选日期所在月份开始并重新聚焦。 */}
          <DayCalendar key={calendar.generation} value={props.value} on_select={(value) => {
            props.on_change(value);
            closeAndRestoreFocus();
          }} />
        </Suspense>
      </div>
    </AnchoredOverlay>
  </div>;
}
