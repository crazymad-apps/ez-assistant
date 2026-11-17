import { useEffect, useRef } from "react";
import type { SidebarPlacement } from "@ez-assistant/protocol";
import type { RootStore } from "../../../stores/RootStore";
import type { SidebarScope } from "../../../stores/SidebarOrderStore";
import styles from "./index.module.scss";

const POINTER_DRAG_DELAY_MS = 120;
const POINTER_DRAG_DISTANCE_PX = 8;
const TOUCH_DRAG_DELAY_MS = 400;
const TOUCH_SCROLL_DISTANCE_PX = 4;

type Candidate = {
  pointer: number;
  pressed_at: number;
  x: number;
  y: number;
  current_x: number;
  current_y: number;
  row: HTMLElement;
  scope: SidebarScope;
  scope_json: string;
  owner: string;
  item: string;
  active: boolean;
  ghost: HTMLElement | null;
  line: HTMLElement | null;
  reference: string | null;
  placement: SidebarPlacement;
};

/** DOM 测量与指针生命周期只属于本侧栏；写入和临时排序交给 Store。 */
export function useSidebarDrag(store: RootStore) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const scroll = ref.current;
    if (!scroll) return;
    let drag: Candidate | null = null;
    let long_press = 0;
    let frame = 0;
    let more_timer = 0;
    let hovered_more: HTMLElement | null = null;
    let suppress_click = false;
    const order = store.sidebar_order;
    const reduced_motion = window.matchMedia?.("(prefers-reduced-motion: reduce)");

    function clearMore() {
      window.clearTimeout(more_timer);
      hovered_more = null;
    }

    function finish(commit = false) {
      const current = drag;
      drag = null;
      window.clearTimeout(long_press);
      cancelAnimationFrame(frame);
      clearMore();
      if (!current) return;
      current.ghost?.remove();
      current.line?.remove();
      delete current.row.dataset.sidebarDragging;
      delete scroll!.dataset.sidebarDragging;
      order.setDragging(null);
      if (current.active) suppress_click = true;
      if (current.row.hasPointerCapture?.(current.pointer)) current.row.releasePointerCapture(current.pointer);
      if (!commit || !current.active || !current.reference) return;
      const ids = rows(current).map((row) => row.dataset.sidebarItem!);
      rememberPositions();
      void order.move(current.scope, current.item, current.reference, current.placement, ids);
    }

    function rows(current: Candidate): HTMLElement[] {
      return [...scroll!.querySelectorAll<HTMLElement>("[data-sidebar-item]")].filter((row) => row.dataset.sidebarScope === current.scope_json && row.getBoundingClientRect().height > 0);
    }

    function update() {
      const current = drag;
      if (!current?.active) return;
      if (!order.enabled(current.scope) || order.key(current.scope) !== current.owner || !current.row.isConnected) { finish(); return; }
      const bounds = scroll!.getBoundingClientRect();
      const inside = current.current_x >= bounds.left && current.current_x <= bounds.right && current.current_y >= bounds.top && current.current_y <= bounds.bottom;
      if (inside) {
        const top = current.current_y - bounds.top;
        const bottom = bounds.bottom - current.current_y;
        if (top < 32) scroll!.scrollTop -= (32 - top) / 3;
        else if (bottom < 32) scroll!.scrollTop += (32 - bottom) / 3;
      }
      current.ghost!.style.transform = `translateY(${current.current_y - current.y}px) scale(1.02)`;
      const hit = inside ? document.elementFromPoint(current.current_x, current.current_y) : null;
      const more = hit?.closest<HTMLElement>("[data-sidebar-more]") ?? null;
      const valid_more = more?.dataset.sidebarMore === current.scope_json ? more : null;
      if (valid_more !== hovered_more) {
        clearMore();
        hovered_more = valid_more;
        if (valid_more) more_timer = window.setTimeout(() => {
          valid_more.querySelector<HTMLButtonElement>("button:not(:disabled)")?.click();
        }, 600);
      }
      let row = hit?.closest<HTMLElement>("[data-sidebar-item]") ?? null;
      if (valid_more) row = rows(current).at(-1) ?? null;
      if (!row || row.dataset.sidebarScope !== current.scope_json || row.dataset.sidebarItem === current.item) {
        current.reference = null;
        current.line!.hidden = true;
      } else {
        const rect = row.getBoundingClientRect();
        current.placement = valid_more || current.current_y > rect.top + rect.height / 2 ? "after" : "before";
        current.reference = row.dataset.sidebarItem!;
        const indent = row.closest<HTMLElement>("[data-indent]")?.dataset.indent === "nested" ? 28 : 0;
        Object.assign(current.line!.style, { top: `${current.placement === "before" ? rect.top : rect.bottom}px`, left: `${rect.left + indent}px`, width: `${rect.width - indent}px` });
        current.line!.hidden = false;
      }
      frame = requestAnimationFrame(update);
    }

    function start() {
      const current = drag;
      if (!current || !current.row.isConnected || !order.enabled(current.scope) || order.key(current.scope) !== current.owner) { finish(); return; }
      current.active = true;
      const rect = current.row.getBoundingClientRect();
      current.ghost = current.row.cloneNode(true) as HTMLElement;
      current.ghost.removeAttribute("id");
      current.ghost.removeAttribute("data-sidebar-item");
      current.ghost.setAttribute("aria-hidden", "true");
      current.ghost.classList.add(styles.drag_ghost);
      Object.assign(current.ghost.style, { top: `${rect.top}px`, left: `${rect.left}px`, width: `${rect.width}px`, height: `${rect.height}px` });
      // 保留根分组与嵌套行原本的缩进。
      const original = current.row.querySelector<HTMLElement>("[data-sidebar-handle]");
      const clone = current.ghost.querySelector<HTMLElement>("[data-sidebar-handle]");
      if (original && clone) clone.style.paddingLeft = getComputedStyle(original).paddingLeft;
      current.line = document.createElement("div");
      current.line.className = styles.drag_line;
      current.line.hidden = true;
      scroll!.append(current.ghost, current.line);
      current.row.dataset.sidebarDragging = "true";
      scroll!.dataset.sidebarDragging = "true";
      current.row.setPointerCapture?.(current.pointer);
      order.setDragging(current.scope, current.item);
      frame = requestAnimationFrame(update);
    }

    function down(event: PointerEvent) {
      suppress_click = false;
      if (drag || event.button !== 0 || !event.isPrimary || !(event.target instanceof Element)) return;
      const handle = event.target.closest<HTMLElement>("[data-sidebar-handle]");
      const row = handle?.closest<HTMLElement>("[data-sidebar-item]");
      if (!row?.dataset.sidebarScope || !row.dataset.sidebarItem) return;
      const scope = JSON.parse(row.dataset.sidebarScope) as SidebarScope;
      if (!order.enabled(scope)) return;
      drag = {
        pointer: event.pointerId,
        pressed_at: event.timeStamp,
        x: event.clientX,
        y: event.clientY,
        current_x: event.clientX,
        current_y: event.clientY,
        row,
        scope,
        scope_json: row.dataset.sidebarScope,
        owner: order.key(scope),
        item: row.dataset.sidebarItem,
        active: false,
        ghost: null,
        line: null,
        reference: null,
        placement: "before",
      };
      if (event.pointerType === "touch") long_press = window.setTimeout(start, TOUCH_DRAG_DELAY_MS);
    }

    function move(event: PointerEvent) {
      if (!drag || event.pointerId !== drag.pointer) return;
      drag.current_x = event.clientX;
      drag.current_y = event.clientY;
      if (!drag.active) {
        const distance = Math.hypot(event.clientX - drag.x, event.clientY - drag.y);
        if (event.pointerType === "touch") {
          if (distance > TOUCH_SCROLL_DISTANCE_PX) finish();
        } else if (distance >= POINTER_DRAG_DISTANCE_PX && event.timeStamp - drag.pressed_at >= POINTER_DRAG_DELAY_MS) {
          start();
        }
      }
      if (drag?.active) event.preventDefault();
    }

    function up(event: PointerEvent) {
      if (event.pointerId !== drag?.pointer) return;
      drag.current_x = event.clientX;
      drag.current_y = event.clientY;
      if (drag.active) { cancelAnimationFrame(frame); update(); }
      finish(true);
    }

    function cancel() { finish(); }

    function key(event: KeyboardEvent) {
      if (event.key === "Escape" && drag) { event.preventDefault(); finish(); }
    }

    function click(event: MouseEvent) {
      if (suppress_click && event.isTrusted) { suppress_click = false; event.preventDefault(); event.stopPropagation(); }
    }

    function touch(event: TouchEvent) {
      if (drag?.active) event.preventDefault();
    }

    function context(event: Event) {
      if (drag) event.preventDefault();
    }

    type RowPosition = { index: number; top: number; scope: string };
    let positions = new Map<HTMLElement, RowPosition>();
    const animations = new WeakMap<HTMLElement, Animation>();

    function layoutTop(row: HTMLElement): number {
      // offsetTop 不受仍在播放的位移动画影响。
      let top = 0;
      for (let node: HTMLElement | null = row; node; node = node.offsetParent as HTMLElement | null) {
        top += node.offsetTop;
      }
      return top;
    }

    function rememberPositions(animate_reorder = false) {
      const next = new Map<HTMLElement, RowPosition>();
      const groups = new Map<string, { top: number; previous_index: number }>();
      const reordered_scopes = new Set<string>();
      for (const row of scroll!.querySelectorAll<HTMLElement>("[data-sidebar-item]")) {
        const scope = row.dataset.sidebarScope;
        if (!scope) continue;
        const top = layoutTop(row);
        let group = groups.get(scope);
        if (!group) {
          group = { top, previous_index: -1 };
          groups.set(scope, group);
        }
        next.set(row, { index: next.size, top: top - group.top, scope });
        const before = positions.get(row);
        if (before?.scope !== scope) continue;
        // 仅共同条目的相对顺序改变才是重排。展开、收起和分页也会改变行坐标。
        if (before.index < group.previous_index) reordered_scopes.add(scope);
        group.previous_index = before.index;
      }
      if (animate_reorder && reordered_scopes.size > 0 && !drag?.active && !reduced_motion?.matches) {
        for (const [row, after] of next) {
          const before = positions.get(row);
          if (!reordered_scopes.has(after.scope) || before?.scope !== after.scope) continue;
          const delta = before.top - after.top;
          if (Math.abs(delta) <= 1 || !row.animate) continue;
          animations.get(row)?.cancel();
          animations.set(row, row.animate([{ transform: `translateY(${delta}px)` }, { transform: "translateY(0)" }], { duration: 120, easing: "ease-out" }));
        }
      }
      positions = next;
    }
    rememberPositions();
    const observer = new MutationObserver(() => rememberPositions(true));
    observer.observe(scroll, { childList: true, subtree: true });
    scroll.addEventListener("pointerdown", down);
    scroll.addEventListener("contextmenu", context);
    scroll.addEventListener("click", click, true);
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("touchmove", touch, { passive: false });
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", key);
    return () => {
      observer.disconnect();
      for (const row of positions.keys()) animations.get(row)?.cancel();
      finish();
      scroll.removeEventListener("pointerdown", down);
      scroll.removeEventListener("contextmenu", context);
      scroll.removeEventListener("click", click, true);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("touchmove", touch);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", key);
    };
  }, [store]);
  return ref;
}
