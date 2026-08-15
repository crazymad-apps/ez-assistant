import {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { Icon } from "../Icon";
import styles from "./index.module.scss";

export type SelectionOption<T extends string> = Readonly<{
  value: T;
  label: string;
  description?: string;
  icon?: ReactNode;
}>;

type SelectionPopoverProps<T extends string> = Readonly<{
  aria_label: string;
  content_width?: "default" | "content";
  disabled?: boolean;
  open: boolean;
  on_open_change: (open: boolean) => void;
  on_select: (value: T) => void;
  options: readonly SelectionOption<T>[];
  selected: T;
  title?: string;
  trigger_content?: ReactNode;
  trigger_class_name?: string;
}>;

export function SelectionPopover<T extends string>(props: SelectionPopoverProps<T>) {
  const listbox_id = useId();
  const trigger_ref = useRef<HTMLButtonElement>(null);
  const popover_ref = useRef<HTMLDivElement>(null);
  const selected_index = Math.max(0, props.options.findIndex((option) => option.value === props.selected));
  const active_index_ref = useRef(selected_index);

  useEffect(() => {
    if (!props.open) {
      return undefined;
    }
    active_index_ref.current = selected_index;
    const selected_node = popover_ref.current?.querySelector<HTMLElement>(`[data-option-index="${selected_index}"]`);
    requestAnimationFrame(() => {
      selected_node?.focus();
      selected_node?.scrollIntoView?.({ block: "nearest" });
    });

    function closeOnOutsidePointer(event: PointerEvent) {
      const target = event.target;
      if (target instanceof Node && !trigger_ref.current?.contains(target) && !popover_ref.current?.contains(target)) {
        props.on_open_change(false);
      }
    }
    document.addEventListener("pointerdown", closeOnOutsidePointer, true);
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer, true);
  }, [props.open, props.on_open_change, selected_index]);

  function handleListKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      props.on_open_change(false);
      trigger_ref.current?.focus();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
      return;
    }
    event.preventDefault();
    const direction = event.key === "ArrowDown" ? 1 : -1;
    const next_index = (active_index_ref.current + direction + props.options.length) % props.options.length;
    active_index_ref.current = next_index;
    const next = popover_ref.current?.querySelector<HTMLElement>(`[data-option-index="${next_index}"]`);
    next?.focus();
    next?.scrollIntoView?.({ block: "nearest" });
  }

  return (
    <>
      <button
        aria-controls={props.open ? listbox_id : undefined}
        aria-expanded={props.open}
        aria-haspopup="listbox"
        aria-label={props.aria_label}
        className={props.trigger_class_name}
        disabled={props.disabled}
        onClick={() => props.on_open_change(!props.open)}
        ref={trigger_ref}
        type="button"
      >
        {props.trigger_content ?? props.options[selected_index]?.label ?? props.selected}
        <Icon name="chevron-down" size={14} />
      </button>
      {props.open && (
        <SelectionPortal
          id={listbox_id}
          content_width={props.content_width ?? "default"}
          on_key_down={handleListKeyDown}
          on_select={(value) => {
            props.on_open_change(false);
            trigger_ref.current?.focus();
            if (value !== props.selected) {
              props.on_select(value);
            }
          }}
          options={props.options}
          popover_ref={popover_ref}
          selected={props.selected}
          title={props.title}
          trigger_ref={trigger_ref}
        />
      )}
    </>
  );
}

function SelectionPortal<T extends string>(props: Readonly<{
  content_width: "default" | "content";
  id: string;
  on_key_down: (event: React.KeyboardEvent<HTMLDivElement>) => void;
  on_select: (value: T) => void;
  options: readonly SelectionOption<T>[];
  popover_ref: React.RefObject<HTMLDivElement | null>;
  selected: T;
  title?: string;
  trigger_ref: React.RefObject<HTMLButtonElement | null>;
}>) {
  const [position, setPosition] = useState({ left: 0, top: 0, ready: false });

  useLayoutEffect(() => {
    function updatePosition() {
      const trigger = props.trigger_ref.current;
      const popover = props.popover_ref.current;
      if (!trigger || !popover) {
        return;
      }
      const trigger_rect = trigger.getBoundingClientRect();
      const popover_rect = popover.getBoundingClientRect();
      const viewport_padding = 8;
      const preferred_left = trigger_rect.left + (trigger_rect.width - popover_rect.width) / 2;
      const left = Math.min(
        Math.max(viewport_padding, preferred_left),
        window.innerWidth - popover_rect.width - viewport_padding,
      );
      const top = Math.max(viewport_padding, trigger_rect.top - popover_rect.height - 6);
      setPosition({ left, top, ready: true });
    }
    updatePosition();
    window.addEventListener("resize", updatePosition);
    document.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      document.removeEventListener("scroll", updatePosition, true);
    };
  }, [props.popover_ref, props.trigger_ref]);

  const overlay_root = document.querySelector<HTMLElement>("#overlay-root") ?? document.body;
  return createPortal(
    <div
      className={styles.popover}
      data-position-ready={position.ready}
      data-width={props.content_width}
      id={props.id}
      onKeyDown={props.on_key_down}
      ref={props.popover_ref}
      role="listbox"
      style={{ left: position.left, top: position.top }}
    >
      {props.title && <strong className={styles.title}>{props.title}</strong>}
      <div className={styles.option_scroll}>
        {props.options.map((option, index) => (
          <button
            aria-selected={option.value === props.selected}
            className={styles.option}
            data-has-icon={Boolean(option.icon)}
            data-option-index={index}
            key={option.value}
            onClick={() => props.on_select(option.value)}
            role="option"
            tabIndex={option.value === props.selected ? 0 : -1}
            type="button"
          >
            {option.icon && <span className={styles.option_icon}>{option.icon}</span>}
            <span className={styles.option_text}>
              <b>{option.label}</b>
              {option.description && <small>{option.description}</small>}
            </span>
            <span aria-hidden="true" className={styles.check_slot}>
              {option.value === props.selected && <Icon className={styles.check} name="check" size={16} />}
            </span>
          </button>
        ))}
      </div>
    </div>,
    overlay_root,
  );
}
