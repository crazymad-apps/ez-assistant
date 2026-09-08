import { useEffect, useLayoutEffect, useRef, type RefObject } from "react";
import { layoutResourceBrowser, type BrowserBounds } from "../../../native-bridge/resourceBrowser";
import type { BrowserController } from "../BrowserController";
import { browserIsOccluded } from "./overlayOcclusion";

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
    let force_layout = false;
    let preview_owner: BrowserController | undefined;
    let sampling: { controller: BrowserController } | null = null;
    let preview_timer: ReturnType<typeof setTimeout> | undefined;

    function samplePreview(controller: BrowserController | undefined) {
      if (sampling?.controller === controller) return;
      clearTimeout(preview_timer);
      sampling = controller ? { controller } : null;
      const owner = sampling;
      if (!owner) return;
      async function capture() {
        if (sampling !== owner || disposed || !owner) return;
        await owner.controller.capturePreview();
        if (sampling === owner && !disposed) preview_timer = setTimeout(() => void capture(), 1000);
      }
      void capture();
    }

    async function synchronize() {
      dirty = true;
      if (pending) return;
      pending = true;
      try {
        while (dirty && !disposed) {
          dirty = false;
          const state = current.current;
          const viewport = state.browser_id ? viewports.current.get(state.browser_id) : null;
          const rect = viewport?.getBoundingClientRect();
          const blocked = Boolean(rect && browserIsOccluded(rect, overlays));
          const preview_controller = state.visible && !document.hidden ? state.controller : undefined;
          if (preview_owner !== preview_controller) {
            preview_owner?.clearPreview();
            preview_owner?.setObscured(false);
            preview_owner = preview_controller;
          }
          state.controller?.setObscured(Boolean(preview_controller && blocked));
          const can_show = state.visible && !document.hidden && !blocked && !state.controller?.error
            && rect && rect.width > 0 && rect.height > 0;
          const id = can_show ? state.controller?.native_id ?? null : null;
          const bounds: BrowserBounds | null = id && rect
            ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null;
          const layout_key = JSON.stringify([id, bounds]);
          const reapply = force_layout && id !== null;
          force_layout = false;
          if (!id) samplePreview(undefined);
          if (layout_key !== last_layout || reapply) {
            // 只操作原生网页；HTML 浮层一直独立渲染，内部滚动不能改变整个 portal 的显隐。
            try {
              await layoutResourceBrowser(id, bounds);
              if (disposed) return;
              last_layout = layout_key;
            } catch (failure) {
              if (disposed) return;
              last_layout = "";
              state.controller?.reportError(failure);
            }
          }
          if (id && !dirty && !state.controller?.error) samplePreview(state.controller);
        }
      } finally { pending = false; }
    }

    function schedule() {
      if (disposed) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => { void synchronize(); });
    }
    function windowChanged() {
      // 原生窗口缩放会主动隐藏 child WebView，需要重新应用可见布局。
      force_layout = true;
      schedule();
    }
    function overlayChanged() { observeSizes(); void synchronize(); }
    const observer = new MutationObserver(overlayChanged);
    if (overlays) observer.observe(overlays, {
      childList: true, subtree: true, attributes: true,
      attributeFilter: ["style", "class", "hidden", "data-overlay-region", "data-position-ready"],
    });
    const resize = new ResizeObserver(schedule);
    const observed = new Set<HTMLElement>();
    resize.observe(document.documentElement);
    function observeSizes() {
      const targets = new Set<HTMLElement>([
        ...viewports.current.values(),
        ...overlays?.querySelectorAll<HTMLElement>("[data-overlay-region]") ?? [],
      ]);
      for (const element of observed) {
        if (!targets.has(element)) {
          resize.unobserve(element);
          observed.delete(element);
        }
      }
      for (const element of targets) {
        if (!observed.has(element)) { resize.observe(element); observed.add(element); }
      }
    }
    invalidate.current = () => {
      observeSizes();
      void synchronize();
    };
    window.addEventListener("resize", windowChanged);
    window.addEventListener("scroll", schedule, true);
    window.addEventListener("focus", windowChanged);
    document.addEventListener("visibilitychange", windowChanged);
    overlays?.addEventListener("transitionend", schedule);
    invalidate.current();
    return () => {
      disposed = true;
      samplePreview(undefined);
      preview_owner?.clearPreview();
      preview_owner?.setObscured(false);
      cancelAnimationFrame(frame);
      observer.disconnect();
      resize.disconnect();
      window.removeEventListener("resize", windowChanged);
      window.removeEventListener("scroll", schedule, true);
      window.removeEventListener("focus", windowChanged);
      document.removeEventListener("visibilitychange", windowChanged);
      overlays?.removeEventListener("transitionend", schedule);
      void layoutResourceBrowser(null, null).catch((failure: unknown) => current.current.controller?.reportError(failure));
    };
  }, [overlay_root, viewports]);

  useLayoutEffect(() => { invalidate.current(); }, [controller, visible, browser_id, native_id, error]);
}
