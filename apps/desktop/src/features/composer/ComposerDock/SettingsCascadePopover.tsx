import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "../../../components/Icon";
import { AnchoredOverlay } from "./AnchoredOverlay";
import styles from "./index.module.scss";

export type SettingsCascadeOption = Readonly<{
  description?: string;
  label: string;
  value: string;
}>;

export type SettingsCascadeCategory = Readonly<{
  disabled_reason?: string;
  id: string;
  label: string;
  on_select: (value: string) => Promise<boolean>;
  options: readonly SettingsCascadeOption[];
  selected: string;
  value_label: string;
}>;

type SettingsCascadePopoverProps = Readonly<{
  aria_label: string;
  categories: readonly SettingsCascadeCategory[];
  disabled?: boolean;
  initial_category: string | null;
  on_open_change: (open: boolean) => void;
  open: boolean;
  trigger_class_name: string;
  trigger_content: string;
}>;

/** 两级菜单只组织独立设置项，不把分类关系提升为业务依赖。 */
export function SettingsCascadePopover(props: SettingsCascadePopoverProps) {
  const menu_id = useId();
  const trigger_ref = useRef<HTMLButtonElement>(null);
  const overlay_ref = useRef<HTMLDivElement>(null);
  const secondary_ref = useRef<HTMLDivElement>(null);
  const [active_category_index, setActiveCategoryIndex] = useState(0);
  const [secondary_category_id, setSecondaryCategoryId] = useState<string | null>(null);
  const [active_option_index, setActiveOptionIndex] = useState(0);
  const [selecting, setSelecting] = useState(false);
  const [secondary_position, setSecondaryPosition] = useState<SecondaryPosition>({ side: "right", top: -6 });
  const secondary_category = props.categories.find((candidate) => candidate.id === secondary_category_id);

  useEffect(() => {
    if (!props.open) return undefined;
    const requested_index = props.initial_category
      ? props.categories.findIndex((candidate) => candidate.id === props.initial_category)
      : -1;
    const initial_index = requested_index >= 0 ? requested_index : 0;
    const requested_category = props.categories[initial_index];
    const can_open_requested = requested_index >= 0 && !requested_category?.disabled_reason;
    setActiveCategoryIndex(initial_index);
    setSecondaryCategoryId(can_open_requested ? requested_category?.id ?? null : null);
    setActiveOptionIndex(selectedOptionIndex(requested_category));
    const focus_frame = requestAnimationFrame(() => {
      const selector = can_open_requested
        ? `[data-setting-option-index="${selectedOptionIndex(requested_category)}"]`
        : `[data-setting-category-index="${initial_index}"]`;
      overlay_ref.current?.querySelector<HTMLElement>(selector)?.focus();
    });
    return () => cancelAnimationFrame(focus_frame);
  }, [props.categories, props.initial_category, props.open]);

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
    setSecondaryCategoryId(next.id);
    const option_index = selectedOptionIndex(next);
    setActiveOptionIndex(option_index);
    requestAnimationFrame(() => {
      overlay_ref.current?.querySelector<HTMLElement>(`[data-setting-option-index="${option_index}"]`)?.focus();
    });
  }

  function focusCategory(index: number) {
    setActiveCategoryIndex(index);
    overlay_ref.current?.querySelector<HTMLElement>(`[data-setting-category-index="${index}"]`)?.focus();
  }

  function moveCategory(direction: 1 | -1, from_index: number) {
    if (props.categories.length === 0) return;
    let next = from_index;
    for (let checked = 0; checked < props.categories.length; checked += 1) {
      next = (next + direction + props.categories.length) % props.categories.length;
      if (!props.categories[next]?.disabled_reason) {
        focusCategory(next);
        return;
      }
    }
  }

  function handlePrimaryKeyDown(event: React.KeyboardEvent<HTMLButtonElement>, index: number) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeAndRestoreFocus();
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveCategory(event.key === "ArrowDown" ? 1 : -1, index);
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
      setSecondaryCategoryId(null);
      focusCategory(active_category_index);
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

  async function selectOption(option: SettingsCascadeOption) {
    if (!secondary_category || selecting || option.value === secondary_category.selected) {
      closeAndRestoreFocus();
      return;
    }
    setSelecting(true);
    const succeeded = await secondary_category.on_select(option.value);
    setSelecting(false);
    if (succeeded) closeAndRestoreFocus();
  }

  return (
    <>
      <button
        aria-controls={props.open ? menu_id : undefined}
        aria-expanded={props.open}
        aria-haspopup="menu"
        aria-label={props.aria_label}
        className={props.trigger_class_name}
        disabled={props.disabled}
        onClick={() => props.on_open_change(!props.open)}
        ref={trigger_ref}
        type="button"
      >
        <span>{props.trigger_content}</span>
        <Icon name="chevron-down" size={14} />
      </button>
        <AnchoredOverlay
          aria_label={props.aria_label}
          class_name={styles.settings_cascade}
          on_request_close={closeAndRestoreFocus}
          open={props.open}
          overlay_ref={overlay_ref}
          trigger_ref={trigger_ref}
        >
          <div className={styles.settings_primary} id={menu_id} role="menu">
            {props.categories.map((item, index) => (
              <button
                aria-disabled={Boolean(item.disabled_reason)}
                className={styles.settings_category}
                data-active={active_category_index === index}
                data-setting-category-index={index}
                key={item.id}
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
            ))}
          </div>
          {secondary_category && (
            <div
              aria-label={secondary_category.label}
              className={styles.settings_secondary}
              data-side={secondary_position.side}
              ref={secondary_ref}
              role="menu"
              style={{ top: secondary_position.top }}
            >
              <strong>{secondary_category.label}</strong>
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
  side: "left" | "right";
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
