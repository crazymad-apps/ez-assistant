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
import { useInputMethodGuard } from "../InputMethodGuard";
import { usePresence, type Presence } from "../Presence";
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
  editable?: boolean;
  open: boolean;
  on_open_change: (open: boolean) => void;
  on_select: (value: T) => void;
  options: readonly SelectionOption<T>[];
  placeholder?: string;
  selected: T;
  title?: string;
  trigger_content?: ReactNode;
  trigger_class_name?: string;
  trigger_variant?: "unstyled" | "field" | "compact";
}>;

export function SelectionPopover<T extends string>(props: SelectionPopoverProps<T>) {
  const input_method = useInputMethodGuard();
  const listbox_id = useId();
  const trigger_ref = useRef<HTMLElement>(null);
  const focus_target_ref = useRef<HTMLElement>(null);
  const popover_ref = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const visible_options = props.editable && query
    ? props.options.filter((option) => `${option.label} ${option.value}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()))
    : props.options;
  const selected_index = Math.max(0, visible_options.findIndex((option) => option.value === props.selected));
  const listbox_visible = props.open && visible_options.length > 0;
  const listbox_presence = usePresence(listbox_visible, 90);
  const active_index_ref = useRef(selected_index);
  const focus_index_on_open_ref = useRef<number | null>(null);
  const [editable_active_index, setEditableActiveIndex] = useState(selected_index);

  useEffect(() => {
    if (!props.open) {
      return undefined;
    }
    const initial_index = focus_index_on_open_ref.current ?? selected_index;
    focus_index_on_open_ref.current = null;
    active_index_ref.current = initial_index;
    setEditableActiveIndex(initial_index);
    const selected_node = popover_ref.current?.querySelector<HTMLElement>(`[data-option-index="${initial_index}"]`);
    const focus_frame = requestAnimationFrame(() => {
      if (!props.editable) {
        selected_node?.focus();
      }
      selected_node?.scrollIntoView?.({ block: "nearest" });
    });

    function closeOnOutsidePointer(event: PointerEvent) {
      const target = event.target;
      if (target instanceof Node && !trigger_ref.current?.contains(target) && !popover_ref.current?.contains(target)) {
        props.on_open_change(false);
      }
    }
    document.addEventListener("pointerdown", closeOnOutsidePointer, true);
    return () => {
      cancelAnimationFrame(focus_frame);
      document.removeEventListener("pointerdown", closeOnOutsidePointer, true);
    };
  }, [listbox_presence.mounted, props.editable, props.open, props.on_open_change, selected_index]);

  useEffect(() => {
    if (!props.open) {
      setQuery("");
    }
  }, [props.open]);

  function handleListKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      props.on_open_change(false);
      focus_target_ref.current?.focus();
      return;
    }
    if (visible_options.length === 0) {
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Home" && event.key !== "End") {
      return;
    }
    event.preventDefault();
    let next_index = event.key === "Home" ? 0 : visible_options.length - 1;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      const direction = event.key === "ArrowDown" ? 1 : -1;
      next_index = (active_index_ref.current + direction + visible_options.length) % visible_options.length;
    }
    active_index_ref.current = next_index;
    const next = popover_ref.current?.querySelector<HTMLElement>(`[data-option-index="${next_index}"]`);
    next?.focus();
    next?.scrollIntoView?.({ block: "nearest" });
  }

  function selectOption(value: T) {
    props.on_open_change(false);
    setQuery("");
    focus_target_ref.current?.focus();
    if (value !== props.selected) {
      props.on_select(value);
    }
  }

  function handleEditableKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (input_method.shouldIgnoreKeyDown(event)) {
      return;
    }
    if (event.key === "Escape" && props.open) {
      event.preventDefault();
      props.on_open_change(false);
      return;
    }
    if (event.key === "Enter" && props.open && visible_options[editable_active_index]) {
      event.preventDefault();
      selectOption(visible_options[editable_active_index].value);
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") {
      return;
    }
    event.preventDefault();
    if (!props.open) {
      props.on_open_change(true);
    }
    const direction = event.key === "ArrowDown" ? 1 : -1;
    let current_index = editable_active_index;
    if (!props.open) {
      current_index = event.key === "ArrowDown" ? -1 : 0;
    }
    const next_index = visible_options.length
      ? (current_index + direction + visible_options.length) % visible_options.length
      : 0;
    active_index_ref.current = next_index;
    setEditableActiveIndex(next_index);
  }

  return (
    <>
      {props.editable ? (
        <div
          className={[styles.trigger, props.trigger_class_name].filter(Boolean).join(" ")}
          data-variant={props.trigger_variant ?? "field"}
          ref={(node) => { trigger_ref.current = node; }}
        >
          <input
            aria-activedescendant={listbox_visible && visible_options[editable_active_index]
              ? `${listbox_id}-option-${editable_active_index}`
              : undefined}
            aria-autocomplete="list"
            aria-controls={listbox_visible ? listbox_id : undefined}
            aria-expanded={listbox_visible}
            aria-haspopup="listbox"
            aria-label={props.aria_label}
            disabled={props.disabled}
            onChange={(event) => {
              const value = event.currentTarget.value;
              setQuery(value);
              setEditableActiveIndex(0);
              props.on_select(value as T);
              if (!props.open) props.on_open_change(true);
            }}
            onClick={() => {
              setQuery("");
              if (!props.open) props.on_open_change(true);
            }}
            onCompositionEnd={input_method.onCompositionEnd}
            onCompositionStart={input_method.onCompositionStart}
            onKeyDown={handleEditableKeyDown}
            onKeyUp={input_method.onKeyUp}
            placeholder={props.placeholder}
            ref={(node) => { focus_target_ref.current = node; }}
            role="combobox"
            value={props.selected}
          />
          <button
            aria-label={`${props.aria_label}选项`}
            disabled={props.disabled}
            onClick={() => {
              setQuery("");
              props.on_open_change(!props.open);
              focus_target_ref.current?.focus();
            }}
            tabIndex={-1}
            type="button"
          >
            <Icon name="chevron-down" size={14} />
          </button>
        </div>
      ) : (
        <button
          aria-controls={listbox_visible ? listbox_id : undefined}
          aria-expanded={listbox_visible}
          aria-haspopup="listbox"
          aria-label={props.aria_label}
          className={[styles.trigger, props.trigger_class_name].filter(Boolean).join(" ")}
          data-variant={props.trigger_variant ?? "unstyled"}
          disabled={props.disabled}
          onClick={() => props.on_open_change(!props.open)}
          onKeyDown={(event) => {
            if ((event.key === "ArrowDown" || event.key === "ArrowUp") && !props.open && visible_options.length > 0) {
              event.preventDefault();
              focus_index_on_open_ref.current = event.key === "ArrowDown" ? selected_index : visible_options.length - 1;
              props.on_open_change(true);
            }
          }}
          ref={(node) => {
            trigger_ref.current = node;
            focus_target_ref.current = node;
          }}
          type="button"
        >
          {props.trigger_content ?? visible_options[selected_index]?.label ?? props.selected}
          <Icon name="chevron-down" size={14} />
        </button>
      )}
      <SelectionPortal
          open={listbox_visible}
          presence={listbox_presence}
          id={listbox_id}
          aria_label={props.aria_label}
          content_width={props.content_width ?? "default"}
          on_key_down={handleListKeyDown}
          active_index={props.editable ? editable_active_index : null}
          match_trigger_width={props.trigger_variant === "field"}
          on_active_index_change={setEditableActiveIndex}
          on_select={selectOption}
          options={visible_options}
          popover_ref={popover_ref}
          selected={props.selected}
          title={props.title}
          trigger_ref={trigger_ref}
        />
    </>
  );
}

function SelectionPortal<T extends string>(props: Readonly<{
  active_index: number | null;
  aria_label: string;
  content_width: "default" | "content";
  id: string;
  match_trigger_width: boolean;
  on_active_index_change: (index: number) => void;
  on_key_down: (event: React.KeyboardEvent<HTMLDivElement>) => void;
  on_select: (value: T) => void;
  open: boolean;
  options: readonly SelectionOption<T>[];
  popover_ref: React.RefObject<HTMLDivElement | null>;
  presence: Presence;
  selected: T;
  title?: string;
  trigger_ref: React.RefObject<HTMLElement | null>;
}>) {
  const [position, setPosition] = useState({ left: 0, top: 0, min_width: 0, ready: false });

  useLayoutEffect(() => {
    if (!props.open) return undefined;
    function updatePosition() {
      const trigger = props.trigger_ref.current;
      const popover = props.popover_ref.current;
      if (!trigger || !popover) {
        return;
      }
      const trigger_rect = trigger.getBoundingClientRect();
      const popover_rect = popover.getBoundingClientRect();
      const viewport_padding = 8;
      const popover_width = Math.max(popover_rect.width, props.match_trigger_width ? trigger_rect.width : 0);
      const preferred_left = props.match_trigger_width
        ? trigger_rect.left
        : trigger_rect.left + (trigger_rect.width - popover_width) / 2;
      const left = Math.min(
        Math.max(viewport_padding, preferred_left),
        window.innerWidth - popover_width - viewport_padding,
      );
      const gap = 6;
      const room_above = trigger_rect.top - viewport_padding;
      const preferred_top = room_above >= popover_rect.height + gap
        ? trigger_rect.top - popover_rect.height - gap
        : trigger_rect.bottom + gap;
      const top = Math.min(
        Math.max(viewport_padding, preferred_top),
        window.innerHeight - popover_rect.height - viewport_padding,
      );
      setPosition({
        left,
        top,
        min_width: props.match_trigger_width ? trigger_rect.width : 0,
        ready: true,
      });
    }
    updatePosition();
    window.addEventListener("resize", updatePosition);
    document.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      document.removeEventListener("scroll", updatePosition, true);
    };
  }, [props.match_trigger_width, props.open, props.popover_ref, props.presence.mounted, props.trigger_ref]);

  if (!props.presence.mounted) return null;

  const overlay_root = document.querySelector<HTMLElement>("#overlay-root") ?? document.body;
  return createPortal(
    <div
      className={styles.popover}
      aria-label={props.aria_label}
      aria-hidden={props.presence.state === "exiting" ? true : undefined}
      data-position-ready={position.ready}
      data-presence={props.presence.state}
      data-width={props.content_width}
      id={props.id}
      inert={props.presence.state === "exiting" ? true : undefined}
      onKeyDown={props.on_key_down}
      ref={props.popover_ref}
      role="listbox"
      style={{ left: position.left, minWidth: position.min_width || undefined, top: position.top }}
      onTransitionEnd={props.presence.onTransitionEnd}
    >
      {props.title && <strong className={styles.title}>{props.title}</strong>}
      <div className={styles.option_scroll}>
        {props.options.map((option, index) => (
          <button
            aria-selected={option.value === props.selected}
            className={styles.option}
            data-active={props.active_index === index}
            data-has-icon={Boolean(option.icon)}
            data-option-index={index}
            id={`${props.id}-option-${index}`}
            key={option.value}
            onMouseEnter={() => props.on_active_index_change(index)}
            onPointerDown={(event) => {
              if (props.active_index !== null) event.preventDefault();
            }}
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
