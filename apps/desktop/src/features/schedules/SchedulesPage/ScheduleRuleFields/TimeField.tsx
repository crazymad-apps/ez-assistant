import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { AnchoredOverlay } from "../../../../components/AnchoredOverlay";
import { Button } from "../../../../components/Button";
import { Icon } from "../../../../components/Icon";
import form_styles from "../index.module.scss";
import styles from "./index.module.scss";

const time_pattern = "(?:[01][0-9]|2[0-3]):[0-5][0-9]";
const schedule_time = new RegExp(`^${time_pattern}$`);

export function isScheduleTime(value: string): boolean {
  return schedule_time.test(value);
}

export function ScheduleTimeField(props: Readonly<{ value: string; disabled: boolean; on_change: (time: string) => void }>) {
  const [open, setOpen] = useState(false);
  const [touched, setTouched] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const overlay = useRef<HTMLDivElement>(null);
  const input_id = useId();
  const panel_id = useId();
  const error_id = useId();
  const expanded = open && !props.disabled;
  const [hour, minute] = (isScheduleTime(props.value) ? props.value : "00:00").split(":");
  const invalid = touched && !isScheduleTime(props.value);
  const close = useCallback(() => setOpen(false), []);

  useEffect(() => {
    if (!expanded) return;
    // 按当前选中值定位两列，避免每次打开都从 00 开始滚动。
    const frame = requestAnimationFrame(() => {
      const lists = overlay.current?.querySelectorAll<HTMLElement>("[data-time-list]");
      lists?.forEach((list) => {
        const choice = list.querySelector<HTMLInputElement>("input:checked");
        const row = choice?.parentElement;
        if (row) list.scrollTop = row.offsetTop - (list.clientHeight - row.offsetHeight) / 2;
      });
      lists?.[0]?.querySelector<HTMLInputElement>("input:checked")?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(frame);
  }, [expanded]);

  function closeAndRestoreFocus() {
    close();
    trigger.current?.focus({ preventScroll: true });
  }

  function handleEscape(event: KeyboardEvent) {
    if (event.key === "Escape" && expanded) {
      event.preventDefault();
      event.stopPropagation();
      closeAndRestoreFocus();
    }
  }

  return <div className={form_styles.field}>
    <label htmlFor={input_id}>时间（设备当地时间）</label>
    <div className={styles.time_field} data-control="field" data-open={expanded || undefined}
      aria-invalid={invalid || undefined} ref={anchor} onKeyDown={handleEscape}
      onBlur={(event) => {
        const next = event.relatedTarget;
        if (next && !anchor.current?.contains(next) && !overlay.current?.contains(next)) close();
      }}>
      <input id={input_id} type="text" autoComplete="off" spellCheck={false} placeholder="HH:mm"
        required disabled={props.disabled} maxLength={5} pattern={time_pattern} value={props.value}
        aria-invalid={invalid || undefined} aria-describedby={invalid ? error_id : undefined}
        onChange={(event) => props.on_change(event.target.value)} onBlur={() => setTouched(true)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" && !event.nativeEvent.isComposing) {
            event.preventDefault();
            setOpen(true);
          }
        }} />
      <button className={styles.time_trigger} aria-label="选择时间" aria-haspopup="dialog" aria-expanded={expanded}
        aria-controls={expanded ? panel_id : undefined} type="button" disabled={props.disabled}
        onClick={() => setOpen(!expanded)} ref={trigger}>
        <Icon name="clock" size={16} />
      </button>
    </div>
    {invalid && <span className={styles.time_error} id={error_id}>输入 24 小时时间，格式为 HH:mm。</span>}
    <AnchoredOverlay aria_label="选择时间" class_name={styles.time_popover} open={expanded}
      trigger_ref={anchor} overlay_ref={overlay} on_request_close={close}
      region_props={{ role: "dialog", onKeyDown: handleEscape, onBlur: (event) => {
        const next = event.relatedTarget;
        if (next && !anchor.current?.contains(next) && !overlay.current?.contains(next)) close();
      } }}>
      <div id={panel_id}>
        <div className={styles.time_options}>
          {([["小时", "时", 24, hour], ["分钟", "分", 60, minute]] as const).map(([label, unit, count, selected]) =>
            <fieldset className={styles.time_column} key={unit}>
              <legend>{label}</legend>
              <div className={styles.time_list} data-time-list>
                {Array.from({ length: count }, (_, index) => {
                  const option = String(index).padStart(2, "0");
                  return <label className={styles.time_choice} key={option}>
                    <input type="radio" name={`${panel_id}-${unit}`} aria-label={`${index}${unit}`} checked={selected === option}
                      onChange={() => props.on_change(unit === "时" ? `${option}:${minute}` : `${hour}:${option}`)} />
                    <span>{option}</span>
                  </label>;
                })}
              </div>
            </fieldset>)}
        </div>
        <div className={styles.time_footer}>
          <output>{hour}:{minute}</output>
          <Button size="small" variant="primary" onClick={() => {
            if (!isScheduleTime(props.value)) props.on_change(`${hour}:${minute}`);
            closeAndRestoreFocus();
          }}>确定</Button>
        </div>
      </div>
    </AnchoredOverlay>
  </div>;
}
