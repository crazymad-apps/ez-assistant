import {
  createContext,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type HTMLAttributes,
  type PropsWithChildren,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import {
  buttonVisualProps,
  type ButtonVisualProps,
} from "../Button";
import { usePresence } from "../Presence";
import styles from "./index.module.scss";

type DropdownMenuContextValue = {
  readonly content_id: string;
  readonly focus_item_on_open_ref: RefObject<"first" | "last" | null>;
  readonly menu_ref: RefObject<HTMLDivElement | null>;
  readonly open: boolean;
  readonly setOpen: (open: boolean) => void;
  readonly trigger_ref: RefObject<HTMLButtonElement | null>;
};

type DropdownMenuProps = PropsWithChildren<{
  readonly className?: string;
}>;

type DropdownMenuTriggerProps = ButtonHTMLAttributes<HTMLButtonElement> & ButtonVisualProps;

type DropdownMenuContentProps = PropsWithChildren<
  Omit<HTMLAttributes<HTMLDivElement>, "role"> & {
    readonly align?: "start" | "end";
  }
>;

type DropdownMenuItemProps = PropsWithChildren<
  Omit<ButtonHTMLAttributes<HTMLButtonElement>, "onClick"> & {
    readonly onSelect?: () => void;
  }
>;

const DropdownMenuContext = createContext<DropdownMenuContextValue | null>(null);

export function DropdownMenu(props: DropdownMenuProps) {
  const [open, setOpen] = useState(false);
  const content_id = useId();
  const focus_item_on_open_ref = useRef<"first" | "last" | null>(null);
  const trigger_ref = useRef<HTMLButtonElement>(null);
  const menu_ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }
      if (!trigger_ref.current?.contains(target) && !menu_ref.current?.contains(target)) {
        setOpen(false);
      }
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape") {
        return;
      }
      event.preventDefault();
      setOpen(false);
      trigger_ref.current?.focus();
    }

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [open]);

  const context: DropdownMenuContextValue = {
    content_id,
    focus_item_on_open_ref,
    menu_ref,
    open,
    setOpen,
    trigger_ref,
  };

  return (
    <DropdownMenuContext.Provider value={context}>
      <span className={[styles.root, props.className].filter(Boolean).join(" ")} data-state={open ? "open" : "closed"}>
        {props.children}
      </span>
    </DropdownMenuContext.Provider>
  );
}

export function DropdownMenuTrigger(props: DropdownMenuTriggerProps) {
  const menu = useDropdownMenu();
  const {
    className,
    iconOnly,
    onClick,
    onKeyDown,
    size,
    variant,
    ...button_props
  } = props;
  const visual_props = variant
    ? buttonVisualProps({ className, iconOnly, size, variant })
    : { className };

  return (
    <button
      {...button_props}
      {...visual_props}
      aria-controls={menu.open ? menu.content_id : undefined}
      aria-expanded={menu.open}
      aria-haspopup="menu"
      data-state={menu.open ? "open" : "closed"}
      onClick={(event) => {
        onClick?.(event);
        if (!event.defaultPrevented) {
          if (!menu.open) {
            menu.focus_item_on_open_ref.current = event.detail === 0 ? "first" : null;
          }
          menu.setOpen(!menu.open);
        }
      }}
      onKeyDown={(event) => {
        onKeyDown?.(event);
        if (!event.defaultPrevented && !menu.open && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
          event.preventDefault();
          menu.focus_item_on_open_ref.current = event.key === "ArrowDown" ? "first" : "last";
          menu.setOpen(true);
        }
      }}
      ref={menu.trigger_ref}
      type={props.type ?? "button"}
    />
  );
}

export function DropdownMenuContent(props: DropdownMenuContentProps) {
  const menu = useDropdownMenu();
  const content_ref = menu.menu_ref;
  const presence = usePresence(menu.open, 90);
  const [position, setPosition] = useState({ left: 0, top: 0, ready: false, side: "below" as "above" | "below" });

  useLayoutEffect(() => {
    if (!menu.open) {
      return undefined;
    }

    function updatePosition() {
      const trigger = menu.trigger_ref.current;
      const content = content_ref.current;
      if (!trigger || !content) {
        return;
      }
      const trigger_rect = trigger.getBoundingClientRect();
      const content_rect = content.getBoundingClientRect();
      const viewport_padding = 8;
      const gap = 4;
      const preferred_left = props.align === "start"
        ? trigger_rect.left
        : trigger_rect.right - content_rect.width;
      const left = Math.min(
        Math.max(viewport_padding, preferred_left),
        window.innerWidth - content_rect.width - viewport_padding,
      );
      const room_below = window.innerHeight - trigger_rect.bottom - viewport_padding;
      const side = room_below >= content_rect.height + gap ? "below" : "above";
      const top = side === "below"
        ? trigger_rect.bottom + gap
        : Math.max(viewport_padding, trigger_rect.top - content_rect.height - gap);
      setPosition({ left, top, ready: true, side });
    }

    updatePosition();
    window.addEventListener("resize", updatePosition);
    document.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      document.removeEventListener("scroll", updatePosition, true);
    };
  }, [content_ref, menu.open, menu.trigger_ref, presence.mounted, props.align]);

  useEffect(() => {
    if (!menu.open || !position.ready) {
      return;
    }
    const focus_intent = menu.focus_item_on_open_ref.current;
    if (focus_intent) {
      const items = getMenuItems(content_ref.current);
      const target = focus_intent === "first" ? items[0] : items.at(-1);
      target?.focus();
      menu.focus_item_on_open_ref.current = null;
    }
  }, [content_ref, menu.focus_item_on_open_ref, menu.open, position.ready, presence.mounted]);

  if (!presence.mounted) {
    return null;
  }

  const overlay_root = document.querySelector<HTMLElement>("#overlay-root") ?? document.body;
  const { align: _align, className, style, ...content_props } = props;
  return createPortal(
    <div
      {...content_props}
      className={[styles.content, className].filter(Boolean).join(" ")}
      aria-hidden={presence.state === "exiting" ? true : undefined}
      data-position-ready={position.ready}
      data-presence={presence.state}
      data-side={position.side}
      id={menu.content_id}
      inert={presence.state === "exiting" ? true : undefined}
      onKeyDown={(event) => {
        content_props.onKeyDown?.(event);
        if (event.defaultPrevented) {
          return;
        }
        const items = getMenuItems(content_ref.current);
        if (items.length === 0) {
          return;
        }
        const current_index = Math.max(0, items.indexOf(document.activeElement as HTMLButtonElement));
        let next_index: number | null = null;
        if (event.key === "ArrowDown") next_index = (current_index + 1) % items.length;
        if (event.key === "ArrowUp") next_index = (current_index - 1 + items.length) % items.length;
        if (event.key === "Home") next_index = 0;
        if (event.key === "End") next_index = items.length - 1;
        if (next_index !== null) {
          event.preventDefault();
          items[next_index]?.focus();
        }
      }}
      ref={content_ref}
      role="menu"
      style={{ ...style, left: position.left, top: position.top }}
      onTransitionEnd={presence.onTransitionEnd}
    />,
    overlay_root,
  );
}

export function DropdownMenuItem(props: DropdownMenuItemProps) {
  const menu = useDropdownMenu();
  const { className, onSelect, ...button_props } = props;

  return (
    <button
      {...button_props}
      className={[styles.item, className].filter(Boolean).join(" ")}
      onClick={() => {
        onSelect?.();
        menu.setOpen(false);
      }}
      role="menuitem"
      type={props.type ?? "button"}
    />
  );
}

function useDropdownMenu(): DropdownMenuContextValue {
  const context = useContext(DropdownMenuContext);
  if (!context) {
    throw new Error("DropdownMenu compound components must be used within DropdownMenu");
  }
  return context;
}

function getMenuItems(container: HTMLElement | null): HTMLButtonElement[] {
  return container
    ? Array.from(container.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)'))
    : [];
}
