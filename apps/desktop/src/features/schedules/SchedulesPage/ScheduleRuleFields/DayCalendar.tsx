import { useEffect, useRef, type ComponentProps } from "react";
import { DayPicker, type ChevronProps } from "@daypicker/react";
import { zhCN } from "@daypicker/react/locale/zh-CN";
import { Icon } from "../../../../components/Icon";
import styles from "./index.module.scss";

const calendar_classes = {
  root: styles.calendar,
  nav: styles.calendar_nav,
  month_caption: styles.calendar_caption,
  month_grid: styles.calendar_grid,
  day_button: styles.calendar_day,
  selected: styles.calendar_selected,
  outside: styles.calendar_outside,
  today: styles.calendar_today,
};

function Chevron({ orientation }: ChevronProps) {
  return <Icon name={orientation === "left" ? "chevron-left" : "chevron-right"} size={16} />;
}

function MonthButton({ onClick, ...button_props }: ComponentProps<"button">) {
  return <button {...button_props} onClick={(event) => {
    // Safari 点击按钮时不一定给它焦点。翻月后箭头仍可接收 Tab 和 Esc。
    event.currentTarget.focus({ preventScroll: true });
    onClick?.(event);
  }} />;
}

export default function DayCalendar(props: Readonly<{ value: string; on_select: (date: string) => void }>) {
  const content = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      content.current?.querySelector<HTMLButtonElement>('button[tabindex="0"]')?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(frame);
  }, []);

  // 日期只是协议中的日历日。用 UTC 作日历坐标，避免浏览器时区或夏令时转换改变设备应收到的日期。
  const selected = props.value ? new Date(`${props.value}T00:00:00Z`) : undefined;
  const now = new Date();
  const today = new Date(Date.UTC(now.getFullYear(), now.getMonth(), now.getDate()));
  return <div ref={content}>
    <DayPicker mode="single" required locale={zhCN} weekStartsOn={1} timeZone="UTC"
      today={today} defaultMonth={selected} selected={selected} fixedWeeks showOutsideDays
      classNames={calendar_classes} components={{ Chevron, NextMonthButton: MonthButton, PreviousMonthButton: MonthButton }}
      formatters={{ formatCaption: (month) => `${month.getUTCFullYear()} 年 ${month.getUTCMonth() + 1} 月` }}
      onSelect={(day) => { if (day) props.on_select(day.toISOString().slice(0, 10)); }} />
  </div>;
}
