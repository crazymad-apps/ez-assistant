import { useEffect, useRef, type RefObject } from "react";
import type { DesktopController } from "../DesktopController";
import { desktopKey, desktopPoint } from "./inputGeometry";

/** DOM 焦点与坐标适配；停止意图经独立控制面发送，不由视图判断授权。 */
export function useDesktopInput(controller: DesktopController, video: RefObject<HTMLVideoElement | null>, active: boolean) {
  const clearRef = useRef<() => void>(() => {});
  useEffect(() => {
    const element = video.current;
    if (!element) return;
    let frame = 0;
    let lastClick = { at: 0, x: 0, y: 0, button: -1, count: 0 };
    const clickCounts = new Map<number, number>();
    let move: { x: number; y: number } | null = null;
    const buttons = new Set<number>();
    const keys = new Set<string>();
    const clear = () => { cancelAnimationFrame(frame); frame = 0; move = null; buttons.clear(); keys.clear(); clickCounts.clear(); };
    clearRef.current = clear;
    const flush = () => { cancelAnimationFrame(frame); frame = 0; if (move) controller.input({ type: "move", ...move }); move = null; };
    const ready = () => active && controller.control === "controlled" && controller.input_focused && !controller.text_pending;
    const point = (event: PointerEvent) => desktopPoint(element.getBoundingClientRect(), element.videoWidth, element.videoHeight, event.clientX, event.clientY);
    const release = () => { clear(); controller.focusInput(false); };
    const stop = () => { clear(); controller.stopControl(); };
    const focus = () => { clear(); controller.focusInput(true); };
    const blur = () => release();
    const captureLost = () => { if (buttons.size) release(); };
    const hidden = () => { if (document.hidden) stop(); };
    const pointer = (event: PointerEvent) => {
      if (controller.control !== "controlled" || !active || controller.text_pending) return;
      const position = point(event);
      if (!position) {
        if (buttons.size) { flush(); for (const button of buttons) controller.input({ type: "button", button, down: false, clicks: 1 }); buttons.clear(); }
        return;
      }
      if (event.type === "pointerdown") {
        if (event.button > 2) return;
        element.focus({ preventScroll: true });
        controller.focusInput(true);
      }
      if (!ready()) return;
      event.preventDefault();
      move = position;
      if (event.type === "pointermove") { if (!frame) frame = requestAnimationFrame(flush); return; }
      flush();
      if (event.type === "pointerdown" && !buttons.has(event.button)) {
        const now = performance.now();
        const again = lastClick.button === event.button && now - lastClick.at < 500 && Math.hypot(event.clientX - lastClick.x, event.clientY - lastClick.y) < 4;
        const clicks = again && lastClick.count === 1 ? 2 : 1;
        lastClick = { at: now, x: event.clientX, y: event.clientY, button: event.button, count: clicks };
        clickCounts.set(event.button, clicks);
        buttons.add(event.button);
        element.setPointerCapture(event.pointerId);
        controller.input({ type: "button", button: event.button, down: true, clicks });
      } else if (event.type === "pointerup" && buttons.delete(event.button)) {
        controller.input({ type: "button", button: event.button, down: false, clicks: clickCounts.get(event.button) ?? 1 });
      }
    };
    const key = (event: KeyboardEvent) => {
      if (!ready()) return;
      if (event.code === "Escape") { event.preventDefault(); event.stopPropagation(); stop(); return; }
      if (event.isComposing || !desktopKey(event.code)) return;
      event.preventDefault(); event.stopPropagation(); flush();
      const down = event.type === "keydown";
      const held = keys.has(event.code);
      if (!down && !held) return;
      if (down) keys.add(event.code); else keys.delete(event.code);
      controller.input({ type: "key", code: event.code, down, repeat: down && held });
    };
    const wheel = (event: WheelEvent) => {
      if (!ready()) return;
      event.preventDefault(); flush();
      const scale = event.deltaMode === 1 ? 20 : event.deltaMode === 2 ? 600 : 1;
      const clamp = (value: number) => Math.round(Math.max(-1200, Math.min(1200, value * scale)));
      controller.input({ type: "scroll", x: clamp(event.deltaX), y: clamp(event.deltaY) });
    };
    const context = (event: Event) => { if (ready()) event.preventDefault(); };
    for (const type of ["pointerdown", "pointerup", "pointermove"] as const) element.addEventListener(type, pointer);
    element.addEventListener("pointercancel", release);
    element.addEventListener("lostpointercapture", captureLost);
    element.addEventListener("focus", focus);
    element.addEventListener("blur", blur);
    element.addEventListener("keydown", key);
    element.addEventListener("keyup", key);
    element.addEventListener("wheel", wheel, { passive: false });
    element.addEventListener("contextmenu", context);
    element.addEventListener("compositionstart", release);
    window.addEventListener("blur", stop);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      clear();
      for (const type of ["pointerdown", "pointerup", "pointermove"] as const) element.removeEventListener(type, pointer);
      element.removeEventListener("pointercancel", release);
      element.removeEventListener("lostpointercapture", captureLost);
      element.removeEventListener("focus", focus);
      element.removeEventListener("blur", blur);
      element.removeEventListener("keydown", key);
      element.removeEventListener("keyup", key);
      element.removeEventListener("wheel", wheel);
      element.removeEventListener("contextmenu", context);
      element.removeEventListener("compositionstart", release);
      window.removeEventListener("blur", stop);
      document.removeEventListener("visibilitychange", hidden);
      controller.stopControl();
    };
  }, [controller, video, active]);
  useEffect(() => { if (!active) controller.stopControl(); }, [active, controller]);
  useEffect(() => { if (controller.control !== "controlled") clearRef.current(); }, [controller.control]);
}
