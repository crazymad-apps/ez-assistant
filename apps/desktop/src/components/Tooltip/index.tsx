import {
  cloneElement,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactElement,
} from "react";
import { createPortal } from "react-dom";
import styles from "./index.module.scss";

type TooltipChildProps = {
  readonly "aria-describedby"?: string;
};

function resolveAnchor(current_target: HTMLElement, event_target: EventTarget | null) {
  if (event_target instanceof Element) {
    const interactive_ancestor = event_target.closest(
      "button, a, input, select, textarea, [tabindex]",
    );
    if (interactive_ancestor instanceof HTMLElement) {
      return interactive_ancestor;
    }
  }
  return current_target.firstElementChild instanceof HTMLElement
    ? current_target.firstElementChild
    : current_target;
}

export function Tooltip(props: Readonly<{
  children: ReactElement<TooltipChildProps>;
  content: string;
}>) {
  const tooltip_id = useId();
  const anchor_ref = useRef<HTMLElement | null>(null);
  const tooltip_ref = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ left: 0, top: 0, ready: false });

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
      }
    }

    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [open]);

  useLayoutEffect(() => {
    if (!open) {
      return undefined;
    }

    function updatePosition() {
      const anchor = anchor_ref.current;
      const tooltip = tooltip_ref.current;
      if (!anchor || !tooltip) {
        return;
      }
      const anchor_rect = anchor.getBoundingClientRect();
      const tooltip_rect = tooltip.getBoundingClientRect();
      const viewport_padding = 8;
      const gap = 6;
      const preferred_left = anchor_rect.left + (anchor_rect.width - tooltip_rect.width) / 2;
      const left = Math.min(
        Math.max(viewport_padding, preferred_left),
        window.innerWidth - tooltip_rect.width - viewport_padding,
      );
      const top = anchor_rect.top >= tooltip_rect.height + gap + viewport_padding
        ? anchor_rect.top - tooltip_rect.height - gap
        : anchor_rect.bottom + gap;
      setPosition({ left, top, ready: true });
    }

    updatePosition();
    window.addEventListener("resize", updatePosition);
    document.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      document.removeEventListener("scroll", updatePosition, true);
    };
  }, [open]);

  const described_by = [props.children.props["aria-describedby"], open ? tooltip_id : null]
    .filter(Boolean)
    .join(" ") || undefined;
  const child = cloneElement(props.children, { "aria-describedby": described_by });
  const overlay_root = document.querySelector<HTMLElement>("#overlay-root") ?? document.body;

  return (
    <span
      className={styles.anchor}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) {
          setOpen(false);
        }
      }}
      onFocus={(event) => {
        anchor_ref.current = resolveAnchor(event.currentTarget, event.target);
        setOpen(true);
      }}
      onPointerEnter={(event) => {
        anchor_ref.current = resolveAnchor(event.currentTarget, event.target);
        setOpen(true);
      }}
      onPointerLeave={() => setOpen(false)}
    >
      {child}
      {open && createPortal(
        <span
          className={styles.tooltip}
          data-position-ready={position.ready}
          id={tooltip_id}
          ref={tooltip_ref}
          role="tooltip"
          style={{ left: position.left, top: position.top }}
        >
          {props.content}
        </span>,
        overlay_root,
      )}
    </span>
  );
}
