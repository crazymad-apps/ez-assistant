import { useEffect, useLayoutEffect, useRef, type RefObject } from "react";
import { layoutResourceBrowser, type BrowserBounds } from "../../../native-bridge/resourceBrowser";
import type { BrowserController } from "../BrowserController";

/** 同时只显示一个原生页面；Shell 显式提供正文测量节点与浮层 portal。 */
export function useBrowserSurface(
  controller: BrowserController | undefined,
  visible: boolean,
  viewports: RefObject<Map<string, HTMLDivElement>>,
  browser_id: string | null,
  overlay_root: RefObject<HTMLDivElement | null>,
): void {
  const current = useRef({ controller, visible, browser_id });
  current.current = { controller, visible, browser_id };
  const invalidate = useRef<() => void>(() => {});
  const native_id = controller?.native_id;
  const error = controller?.error;

  useEffect(() => {
    const overlays = overlay_root.current;
    let disposed = false;
    let pending = false;
    let dirty = false;
    let frame = 0;
    let last_layout = "";

    async function synchronize() {
      dirty = true;
      if (overlays?.childElementCount && last_layout !== JSON.stringify([null, null])) overlays.style.visibility = "hidden";
      if (pending) return;
      pending = true;
      try {
        while (dirty && !disposed) {
          dirty = false;
          const state = current.current;
          const blocked = Boolean(overlays?.childElementCount);
          const viewport = state.browser_id ? viewports.current.get(state.browser_id) : null;
          const rect = viewport?.getBoundingClientRect();
          const can_show = state.visible && !document.hidden && !blocked && !state.controller?.error
            && rect && rect.width > 0 && rect.height > 0;
          const id = can_show ? state.controller?.native_id ?? null : null;
          const bounds: BrowserBounds | null = id && rect
            ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null;
          const layout_key = JSON.stringify([id, bounds]);
          if (layout_key !== last_layout) {
            // 原生子视图高于主 WebView 的 HTML，先完成隐藏再绘制 HTML 浮层，避免穿透。
            if (blocked && overlays) overlays.style.visibility = "hidden";
            try {
              await layoutResourceBrowser(id, bounds);
              last_layout = layout_key;
            } catch (failure) {
              last_layout = "";
              state.controller?.reportError(failure);
            }
          }
          if (overlays) overlays.style.removeProperty("visibility");
        }
      } finally { pending = false; }
    }

    function schedule() {
      if (disposed) return;
      last_layout = "";
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => { void synchronize(); });
    }
    function overlayChanged() { void synchronize(); }
    const observer = new MutationObserver(overlayChanged);
    if (overlays) observer.observe(overlays, { childList: true, subtree: true });
    const resize = new ResizeObserver(schedule);
    const observed = new Set<HTMLDivElement>();
    resize.observe(document.documentElement);
    invalidate.current = () => {
      for (const viewport of observed) {
        if (![...viewports.current.values()].includes(viewport)) {
          resize.unobserve(viewport);
          observed.delete(viewport);
        }
      }
      for (const viewport of viewports.current.values()) {
        if (!observed.has(viewport)) { resize.observe(viewport); observed.add(viewport); }
      }
      void synchronize();
    };
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", schedule, true);
    window.addEventListener("focus", schedule);
    document.addEventListener("visibilitychange", schedule);
    invalidate.current();
    return () => {
      disposed = true;
      cancelAnimationFrame(frame);
      observer.disconnect();
      resize.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
      window.removeEventListener("focus", schedule);
      document.removeEventListener("visibilitychange", schedule);
      if (overlays) overlays.style.removeProperty("visibility");
      void layoutResourceBrowser(null, null).catch((failure: unknown) => current.current.controller?.reportError(failure));
    };
  }, [overlay_root, viewports]);

  useLayoutEffect(() => { invalidate.current(); }, [controller, visible, browser_id, native_id, error]);
}
