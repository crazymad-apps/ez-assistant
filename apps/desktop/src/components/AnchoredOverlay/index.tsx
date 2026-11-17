import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type HTMLAttributes,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { usePresence } from "../Presence";

type AnchoredOverlayProps = Readonly<{
  aria_label: string;
  children: ReactNode;
  class_name: string;
  horizontal_align?: "start" | "center";
  on_request_close: () => void;
  open?: boolean;
  overlay_ref: RefObject<HTMLDivElement | null>;
  placement?: "above" | "auto" | "right";
  bounds_ref?: RefObject<HTMLElement | null>;
  region_props?: Pick<HTMLAttributes<HTMLDivElement>, "role" | "onPointerEnter" | "onPointerLeave" | "onBlur" | "onKeyDown">;
  trigger_ref: RefObject<HTMLElement | null>;
}>;

/** 基于锚点定位浮层，统一 Portal、退场和视口边界。 */
export function AnchoredOverlay(props: AnchoredOverlayProps) {
  const open = props.open ?? true;
  const presence = usePresence(open, 90);
  const retained_children_ref = useRef(props.children);
  if (open) retained_children_ref.current = props.children;
  const [position, setPosition] = useState({ left: 0, top: 0, ready: false });

  useLayoutEffect(() => {
    if (!open || !presence.mounted) return undefined;
    function updatePosition() {
      const trigger = props.trigger_ref.current;
      const overlay = props.overlay_ref.current;
      if (!trigger || !overlay) return;

      const viewport_padding = 8;
      const gap = 6;
      const trigger_rect = trigger.getBoundingClientRect();
      if (props.placement === "right") {
        const bounds = props.bounds_ref?.current?.getBoundingClientRect();
        const visual = window.visualViewport;
        const left = trigger_rect.right + 12;
        const right = Math.min(bounds?.right ?? Infinity, (visual ? visual.offsetLeft + visual.width : window.innerWidth) - viewport_padding);
        const top_edge = Math.max(bounds?.top ?? 0, (visual?.offsetTop ?? 0) + viewport_padding);
        const bottom = Math.min(bounds?.bottom ?? Infinity, (visual ? visual.offsetTop + visual.height : window.innerHeight) - viewport_padding);
        if (left < (visual?.offsetLeft ?? 0) + viewport_padding || right - left < 160 || bottom - top_edge < 88) {
          props.on_request_close();
          return;
        }
        overlay.style.maxWidth = `${right - left}px`;
        overlay.style.maxHeight = `${bottom - top_edge}px`;
        const height = overlay.getBoundingClientRect().height;
        const top = Math.min(Math.max(top_edge, trigger_rect.top + (trigger_rect.height - height) / 2), bottom - height);
        setPosition(current => current.ready && current.left === left && current.top === top ? current : { left, top, ready: true });
        return;
      }
      const overlay_rect = overlay.getBoundingClientRect();
      const preferred_left = props.horizontal_align === "center"
        ? trigger_rect.left + ((trigger_rect.width - overlay_rect.width) / 2)
        : trigger_rect.left;
      const left = Math.min(
        Math.max(viewport_padding, preferred_left),
        Math.max(viewport_padding, window.innerWidth - overlay_rect.width - viewport_padding),
      );
      const room_above = trigger_rect.top - viewport_padding;
      const prefer_above = props.placement === "above" || room_above >= overlay_rect.height + gap;
      const preferred_top = prefer_above
        ? trigger_rect.top - overlay_rect.height - gap
        : trigger_rect.bottom + gap;
      const top = Math.min(
        Math.max(viewport_padding, preferred_top),
        Math.max(viewport_padding, window.innerHeight - overlay_rect.height - viewport_padding),
      );
      setPosition({ left, top, ready: true });
    }

    updatePosition();
    window.addEventListener("resize", updatePosition);
    const visual = props.placement === "right" ? window.visualViewport : null;
    visual?.addEventListener("resize", updatePosition);
    visual?.addEventListener("scroll", updatePosition);
    const bounds = props.bounds_ref?.current;
    const observer = bounds && typeof ResizeObserver !== "undefined" ? new ResizeObserver(updatePosition) : null;
    if (bounds) observer?.observe(bounds);
    document.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      visual?.removeEventListener("resize", updatePosition);
      visual?.removeEventListener("scroll", updatePosition);
      observer?.disconnect();
      document.removeEventListener("scroll", updatePosition, true);
    };
  }, [
    props.children,
    props.bounds_ref,
    props.horizontal_align,
    props.overlay_ref,
    props.on_request_close,
    open,
    presence.mounted,
    props.placement,
    props.trigger_ref,
  ]);

  useEffect(() => {
    if (!open) return undefined;
    function closeOnOutsidePointer(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (!props.trigger_ref.current?.contains(target) && !props.overlay_ref.current?.contains(target)) {
        props.on_request_close();
      }
    }
    document.addEventListener("pointerdown", closeOnOutsidePointer, true);
    return () => document.removeEventListener("pointerdown", closeOnOutsidePointer, true);
  }, [open, props.on_request_close, props.overlay_ref, props.trigger_ref]);

  if (!presence.mounted) return null;

  const overlay_root = document.querySelector<HTMLElement>("#overlay-root") ?? document.body;
  return createPortal(
    <div
      {...props.region_props}
      aria-label={props.aria_label}
      aria-hidden={presence.state === "exiting" ? true : undefined}
      className={props.class_name}
      data-position-ready={position.ready}
      data-overlay-region="floating"
      data-presence={presence.state}
      inert={presence.state === "exiting" ? true : undefined}
      onTransitionEnd={presence.onTransitionEnd}
      ref={props.overlay_ref}
      style={{ left: position.left, top: position.top }}
    >
      {open ? props.children : retained_children_ref.current}
    </div>,
    overlay_root,
  );
}
