import { Fragment, type ReactNode, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { Button } from "../Button";
import { Icon } from "../Icon";
import { AnchoredOverlay } from "../AnchoredOverlay";
import styles from "./index.module.scss";

export type SettingsCascadeOption = Readonly<{
  description?: string;
  label: string;
  value: string;
}>;

export type SettingsCascadeCategory = Readonly<{
  separator_before?: boolean;
  hide_title?: boolean;
  disabled_reason?: string;
  id: string;
  label: string;
  on_select: (value: string) => Promise<boolean>;
  options: readonly SettingsCascadeOption[];
  on_open?: () => void;
  content?: ReactNode;
  selected: string;
  value_label: string;
}>;

type SettingsCascadePopoverProps = Readonly<{
  aria_label: string;
  categories: readonly SettingsCascadeCategory[];
  disabled?: boolean;
  clear_action?: Readonly<{ label: string; on_clear: () => Promise<boolean> }>;
  initial_category: string | null;
  on_open_change: (open: boolean) => void;
  open: boolean;
  primary_content?: ReactNode;
  primary_actions?: readonly Readonly<{
    label: string;
    selected?: boolean;
    disabled?: boolean;
    on_select: () => Promise<boolean>;
  }>[];
  footer_content?: ReactNode;
  trigger_class_name: string;
  trigger_content: string;
}>;

/** 两级菜单只组织独立设置项，不把分类关系提升为业务依赖。 */
export function SettingsCascadePopover(props: SettingsCascadePopoverProps) {
  const clear_action = props.clear_action;
  const menu_id = useId();
  const trigger_ref = useRef<HTMLButtonElement>(null);
  const overlay_ref = useRef<HTMLDivElement>(null);
  const secondary_ref = useRef<HTMLDivElement>(null);
  const [active_category_index, setActiveCategoryIndex] = useState(0);
  const [secondary_category_id, setSecondaryCategoryId] = useState<string | null>(null);
  const [active_option_index, setActiveOptionIndex] = useState(0);
  const [selecting, setSelecting] = useState(false);
  const selecting_ref = useRef(false);
  const [selection_error, setSelectionError] = useState<string | null>(null);
  const [secondary_position, setSecondaryPosition] = useState<SecondaryPosition>({ side: "right", top: -6 });
  const interaction = useRef({ open: props.open, epoch: 0 });
  if (interaction.current.open !== props.open) interaction.current = { open: props.open, epoch: interaction.current.epoch + 1 };
  const latest = useRef(props);
  latest.current = props;
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const secondary_category = props.categories.find((candidate) => candidate.id === secondary_category_id);

  useEffect(() => {
    if (!props.open) return undefined;
    setSelectionError(null);
    const categories = latest.current.categories;
    const requested_index = props.initial_category
      ? categories.findIndex((candidate) => candidate.id === props.initial_category)
      : -1;
    const initial_index = requested_index >= 0 ? requested_index : 0;
    const requested_category = categories[initial_index];
    const can_open_requested = requested_index >= 0 && !requested_category?.disabled_reason;
    if (can_open_requested) requested_category?.on_open?.();
    setActiveCategoryIndex(initial_index);
    setSecondaryCategoryId(can_open_requested ? requested_category?.id ?? null : null);
    setActiveOptionIndex(selectedOptionIndex(requested_category));
    const focus_frame = requestAnimationFrame(() => {
      const selector = can_open_requested
        ? `[data-setting-option-index="${selectedOptionIndex(requested_category)}"]`
        : ':is([role="menuitem"], [role="menuitemradio"]):not(:disabled):not([aria-disabled="true"])';
      overlay_ref.current?.querySelector<HTMLElement>(selector)?.focus();
    });
    return () => cancelAnimationFrame(focus_frame);
  }, [props.initial_category, props.open]);

  useLayoutEffect(() => {
    if (!props.open || !secondary_category) return undefined;
    const primary = overlay_ref.current;
    const secondary = secondary_ref.current;
    if (!primary || !secondary) return undefined;
    const measured_primary: HTMLDivElement = primary;
    const measured_secondary: HTMLDivElement = secondary;
    let position_frame = 0;

    function updatePosition() {
      const next = calculateSecondaryPosition(
        measured_primary.getBoundingClientRect(),
        measured_secondary.getBoundingClientRect(),
        window.innerWidth,
        window.innerHeight,
      );
      setSecondaryPosition((current) => positionsEqual(current, next) ? current : next);
    }

    function schedulePositionUpdate() {
      cancelAnimationFrame(position_frame);
      position_frame = requestAnimationFrame(updatePosition);
    }

    // 先完成当前布局测量；再等一级浮层完成锚点定位后复测，避免读取初始的 (0, 0)。
    updatePosition();
    schedulePositionUpdate();
    window.addEventListener("resize", schedulePositionUpdate);
    document.addEventListener("scroll", schedulePositionUpdate, true);
    const resize_observer = typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver(schedulePositionUpdate);
    resize_observer?.observe(measured_primary);
    resize_observer?.observe(measured_secondary);
    return () => {
      cancelAnimationFrame(position_frame);
      window.removeEventListener("resize", schedulePositionUpdate);
      document.removeEventListener("scroll", schedulePositionUpdate, true);
      resize_observer?.disconnect();
    };
  }, [props.open, secondary_category]);

  function closeAndRestoreFocus() {
    interaction.current.epoch += 1;
    props.on_open_change(false);
    setSecondaryCategoryId(null);
    requestAnimationFrame(() => {
      const active_element = document.activeElement;
      if (!active_element || active_element === document.body) {
        trigger_ref.current?.focus();
      }
    });
  }

  function openCategory(index: number) {
    const next = props.categories[index];
    if (!next || next.disabled_reason) return;
    setActiveCategoryIndex(index);
    if (secondary_category_id !== next.id) next.on_open?.();
    interaction.current.epoch += 1;
    setSecondaryCategoryId(next.id);
    const option_index = selectedOptionIndex(next);
    setActiveOptionIndex(option_index);
    requestAnimationFrame(() => {
      overlay_ref.current?.querySelector<HTMLElement>(`[data-setting-option-index="${option_index}"]`)?.focus();
    });
  }

  function returnToPrimary() {
    setSecondaryCategoryId(null);
    requestAnimationFrame(() => focusCategory(active_category_index));
  }

  function focusCategory(index: number) {
    setActiveCategoryIndex(index);
    overlay_ref.current?.querySelector<HTMLElement>(`[data-setting-category-index="${index}"]`)?.focus();
  }

  // 一级动作、分类和页脚共享键盘顺序，slot 不得成为方向键无法到达的孤岛。
  function handlePrimaryMenuKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (event.defaultPrevented) return;
    if (event.key === "Escape") {
      event.preventDefault();
      closeAndRestoreFocus();
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>(
      ':is([role="menuitem"], [role="menuitemradio"]):not(:disabled):not([aria-disabled="true"])',
    ));
    if (!items.length) return;
    event.preventDefault();
    const current = items.findIndex((item) => item === document.activeElement);
    let next = event.key === "Home" ? 0 : items.length - 1;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      next = (current + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
    }
    items[next]?.focus();
  }

  function handlePrimaryKeyDown(event: React.KeyboardEvent<HTMLButtonElement>, index: number) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeAndRestoreFocus();
      return;
    }
    if (event.key === "ArrowRight" || event.key === "Enter") {
      event.preventDefault();
      openCategory(index);
    }
  }

  function handleSecondaryKeyDown(event: React.KeyboardEvent<HTMLButtonElement>, index: number) {
    const options = secondary_category?.options ?? [];
    if (event.key === "Escape" || event.key === "ArrowLeft") {
      event.preventDefault();
      returnToPrimary();
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp" || event.key === "Home" || event.key === "End") {
      event.preventDefault();
      let next = event.key === "Home" ? 0 : options.length - 1;
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        const direction = event.key === "ArrowDown" ? 1 : -1;
        next = (index + direction + options.length) % options.length;
      }
      setActiveOptionIndex(next);
      overlay_ref.current?.querySelector<HTMLElement>(`[data-setting-option-index="${next}"]`)?.focus();
    }
  }

  async function selectAction(action: () => Promise<boolean>) {
    if (selecting_ref.current) return;
    const epoch = interaction.current.epoch;
    selecting_ref.current = true;
    setSelecting(true);
    setSelectionError(null);
    try {
      const succeeded = await action();
      if (mounted.current && succeeded && latest.current.open && interaction.current.epoch === epoch) closeAndRestoreFocus();
    } catch {
      if (mounted.current && latest.current.open && interaction.current.epoch === epoch) setSelectionError("未能保存设置，请重试。");
    } finally {
      selecting_ref.current = false;
      if (mounted.current) setSelecting(false);
    }
  }

  function selectOption(option: SettingsCascadeOption) {
    if (!secondary_category || selecting_ref.current) return;
    if (option.value === secondary_category.selected) {
      closeAndRestoreFocus();
      return;
    }
    void selectAction(() => secondary_category.on_select(option.value));
  }

  return (
    <>
      <span className={styles.trigger_wrapper}>
      <button
        aria-controls={props.open ? menu_id : undefined}
        aria-expanded={props.open}
        aria-haspopup="menu"
        aria-label={props.aria_label}
        className={[styles.trigger, props.trigger_class_name].filter(Boolean).join(" ")}
        disabled={props.disabled}
        onClick={() => props.on_open_change(!props.open)}
        onKeyDown={(event) => {
          if (props.open && event.key === "Escape") { event.preventDefault(); closeAndRestoreFocus(); }
        }}
        ref={trigger_ref}
        type="button"
      >
        <span>{props.trigger_content}</span>
        {clear_action ? <span className={styles.clear_space} /> : <Icon name="chevron-down" size={14} />}
      </button>
      {clear_action && <button type="button" className={styles.clear_trigger}
        aria-label={clear_action.label} title={clear_action.label} disabled={props.disabled || selecting}
        onClick={() => { void selectAction(async () => {
          const cleared = await clear_action.on_clear();
          if (cleared && mounted.current) trigger_ref.current?.focus();
          return cleared;
        }); }}>
        <Icon name="chevron-down" size={14} className={styles.clear_arrow} />
        <Icon name="x" size={14} className={styles.clear_icon} />
      </button>}
      </span>
        <AnchoredOverlay
          aria_label={props.aria_label}
          class_name={styles.settings_cascade}
          on_request_close={closeAndRestoreFocus}
          open={props.open}
          overlay_ref={overlay_ref}
          trigger_ref={trigger_ref}
        >
          <div className={styles.settings_primary} id={menu_id} role="menu" onKeyDown={handlePrimaryMenuKeyDown}>

            {selection_error && <p role="alert">{selection_error}</p>}
            {props.primary_content && <div className={styles.primary_actions}>{props.primary_content}</div>}
            {props.primary_actions?.map((action) => <button key={action.label} type="button"
              className={styles.settings_category} role={action.selected === undefined ? "menuitem" : "menuitemradio"}
              aria-checked={action.selected} disabled={selecting || action.disabled}
              onFocus={() => setActiveCategoryIndex(-1)} onClick={() => void selectAction(action.on_select)}>
              <span><b>{action.label}</b></span>
              {action.selected && <Icon name="check" size={14} className={styles.selected_icon} />}
            </button>)}
            {props.categories.map((item, index) => (
              <Fragment key={item.id}>
              {item.separator_before && <div role="separator" className={styles.separator} />}
              <button
                aria-disabled={Boolean(item.disabled_reason)}
                aria-haspopup="menu"
                aria-expanded={secondary_category_id === item.id}
                className={styles.settings_category}
                data-active={active_category_index === index}
                data-setting-category-index={index}
                onClick={() => openCategory(index)}
                onFocus={() => setActiveCategoryIndex(index)}
                onKeyDown={(event) => handlePrimaryKeyDown(event, index)}
                role="menuitem"
                title={item.disabled_reason}
                type="button"
              >
                <span><b>{item.label}</b><small>{item.disabled_reason ?? item.value_label}</small></span>
                <Icon name="chevron-right" size={14} />
              </button>
              </Fragment>
            ))}
            {props.footer_content && <div className={styles.primary_actions}>{props.footer_content}</div>}
          </div>
          {secondary_category && (
            <div
              aria-label={secondary_category.label}
              className={styles.settings_secondary}
              data-side={secondary_position.side}
              ref={secondary_ref}
              role="menu"
              onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); returnToPrimary(); } }}
              style={{ top: secondary_position.top }}
            >
              {(!secondary_category.hide_title || secondary_position.side === "inline") && <header className={styles.secondary_header}>
                {secondary_position.side === "inline" && <Button aria-label="返回一级" variant="text" onClick={returnToPrimary}><Icon name="chevron-left" size={14} />返回</Button>}
                {!secondary_category.hide_title && <strong>{secondary_category.label}</strong>}
              </header>}
              {secondary_position.side === "inline" && selection_error && <p role="alert">{selection_error}</p>}
              {secondary_category.content && <section className={styles.secondary_content}>{secondary_category.content}</section>}
              <div>
                {secondary_category.options.map((option, index) => (
                  <button
                    aria-checked={option.value === secondary_category.selected}
                    data-active={active_option_index === index}
                    data-setting-option-index={index}
                    disabled={selecting}
                    key={option.value}
                    onClick={() => void selectOption(option)}
                    onFocus={() => setActiveOptionIndex(index)}
                    onKeyDown={(event) => handleSecondaryKeyDown(event, index)}
                    role="menuitemradio"
                    type="button"
                  >
                    <span><b>{option.label}</b>{option.description && <small>{option.description}</small>}</span>
                    <i>{option.value === secondary_category.selected && <Icon name="check" size={14} />}</i>
                  </button>
                ))}
              </div>
            </div>
          )}
        </AnchoredOverlay>
    </>
  );
}

function selectedOptionIndex(category: SettingsCascadeCategory | undefined): number {
  if (!category) return 0;
  return Math.max(0, category.options.findIndex((option) => option.value === category.selected));
}

type SecondaryPosition = Readonly<{
  side: "left" | "right" | "inline";
  top: number;
}>;

function calculateSecondaryPosition(
  primary: DOMRect,
  secondary: DOMRect,
  viewport_width: number,
  viewport_height: number,
): SecondaryPosition {
  const viewport_padding = 8;
  const gap = 6;
  const room_right = viewport_width - primary.right - gap - viewport_padding;
  const room_left = primary.left - gap - viewport_padding;
  const minimum_width = viewport_width <= 720 ? 180 : 220;
  if (Math.max(room_left, room_right) < minimum_width) return { side: "inline", top: 0 };
  const side = secondary.width <= room_right || room_right >= room_left ? "right" : "left";
  const preferred_viewport_top = primary.top - gap;
  const maximum_viewport_top = Math.max(
    viewport_padding,
    viewport_height - secondary.height - viewport_padding,
  );
  const viewport_top = Math.min(
    Math.max(viewport_padding, preferred_viewport_top),
    maximum_viewport_top,
  );
  return { side, top: viewport_top - primary.top };
}

function positionsEqual(current: SecondaryPosition, next: SecondaryPosition): boolean {
  return current.side === next.side && current.top === next.top;
}
