import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent, type RefObject } from "react";
import type { ConversationNavigationItem, MessageId } from "@ez-assistant/protocol";
import { AnchoredOverlay } from "../../../../components/AnchoredOverlay";
import type { ConversationNavigationStore, PreviewSlot } from "../../../../stores/ConversationNavigationStore";
import styles from "./index.module.scss";

const INTENT_DELAY = 150;

type Target = Readonly<{ item: ConversationNavigationItem; button: HTMLButtonElement }>;
export type LocationFeedback = Readonly<{ id: MessageId; loading: boolean; error?: string }>;

export function useNavigationPreview(
  store: ConversationNavigationStore,
  body_ref: RefObject<HTMLDivElement | null>,
  locating: RefObject<object | null>,
) {
  const [target, setTarget] = useState<Target | null>(null);
  const target_ref = useRef<Target | null>(null);
  const trigger_ref = useRef<HTMLButtonElement>(null);
  const overlay_ref = useRef<HTMLDivElement>(null);
  const open_timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const close_timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const pointer_inside = useRef(false);
  const position_feedback = useRef(false);
  const pointer_position = useRef<{ x: number; y: number } | null>(null);
  const restoring_focus = useRef(false);
  const text_id = useId();

  const close = useCallback(() => {
    clearTimeout(open_timer.current);
    clearTimeout(close_timer.current);
    const button = trigger_ref.current;
    if (overlay_ref.current?.contains(document.activeElement)) {
      restoring_focus.current = true;
      button?.focus({ preventScroll: true });
      restoring_focus.current = false;
    }
    target_ref.current = null;
    position_feedback.current = false;
    setTarget(null);
    store.closePreview();
  }, [store]);

  function show(next: Target) {
    if (target_ref.current?.button === next.button) return;
    store.closePreview();
    position_feedback.current = false;
    trigger_ref.current = next.button;
    target_ref.current = next;
    setTarget(next);
  }

  function schedule(next: Target, keyboard: boolean) {
    if (document.querySelector('[data-overlay-region="modal"]')) return;
    clearTimeout(open_timer.current);
    clearTimeout(close_timer.current);
    if (keyboard || target_ref.current) show(next);
    open_timer.current = setTimeout(() => {
      if (!next.button.isConnected) return;
      show(next);
      void store.openPreview(next.item.user_message_id);
    }, INTENT_DELAY);
  }

  function enter(next: Target, x: number, y: number) {
    pointer_inside.current = true;
    // 滚动改变鼠标下的标记时，浏览器也可能派发 pointerenter。
    if (pointer_position.current?.x === x && pointer_position.current.y === y) return;
    pointer_position.current = { x, y };
    schedule(next, false);
  }

  function scheduleClose() {
    if (!target_ref.current) clearTimeout(open_timer.current);
    clearTimeout(close_timer.current);
    close_timer.current = setTimeout(() => {
      const overlay = overlay_ref.current;
      const selection = window.getSelection();
      const has_selection = Boolean(overlay && selection && !selection.isCollapsed && overlay.contains(selection.anchorNode));
      if (!pointer_inside.current && !has_selection && !overlay?.contains(document.activeElement)) close();
    }, INTENT_DELAY);
  }

  function leave(x: number, y: number) {
    pointer_inside.current = false;
    if (pointer_position.current?.x !== x || pointer_position.current.y !== y) pointer_position.current = null;
    scheduleClose();
  }

  function focusControl(): boolean {
    const control = overlay_ref.current?.querySelector<HTMLButtonElement>("button");
    control?.focus({ preventScroll: true });
    return Boolean(control);
  }

  function showFeedback(next: Target) {
    if (!next.button.isConnected || document.querySelector('[data-overlay-region="modal"]')) return;
    if (target_ref.current && target_ref.current.button !== next.button) return;
    clearTimeout(close_timer.current);
    show(next);
    position_feedback.current = true;
    if (store.preview?.user_message_id !== next.item.user_message_id) void store.openPreview(next.item.user_message_id);
  }

  useEffect(() => {
    const body = body_ref.current;
    const closeOnScroll = () => { if (!locating.current && !position_feedback.current) close(); };
    body?.addEventListener("scroll", closeOnScroll, { passive: true });
    body?.addEventListener("wheel", close, { passive: true });
    body?.addEventListener("touchmove", close, { passive: true });
    const closeOnKey = (event: globalThis.KeyboardEvent) => {
      if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key)) close();
    };
    body?.addEventListener("keydown", closeOnKey);
    return () => {
      body?.removeEventListener("scroll", closeOnScroll);
      body?.removeEventListener("wheel", close);
      body?.removeEventListener("touchmove", close);
      body?.removeEventListener("keydown", closeOnKey);
      clearTimeout(open_timer.current);
      clearTimeout(close_timer.current);
    };
  }, [body_ref, close, locating]);

  useEffect(() => {
    if (!target) return;
    const overlay_root = document.querySelector("#overlay-root") ?? document.body;
    const observer = new MutationObserver(() => {
      if (overlay_root.querySelector('[data-overlay-region="modal"]')) close();
    });
    observer.observe(overlay_root, { childList: true, subtree: true });
    const key = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        close();
      }
    };
    document.addEventListener("keydown", key);
    document.addEventListener("selectionchange", scheduleClose);
    return () => {
      observer.disconnect();
      document.removeEventListener("keydown", key);
      document.removeEventListener("selectionchange", scheduleClose);
    };
  }, [close, target]);

  function render(feedback: LocationFeedback | null, retryLocate: (next: Target) => void) {
    const id = target?.item.user_message_id;
    const preview = store.preview?.user_message_id === id ? store.preview : null;
    const location = feedback?.id === id ? feedback : null;
    const assistant_id = target?.item.assistant_message_id;
    const assistant = preview ? preview.assistant : assistant_id
      ? store.cachedPreview(assistant_id) ?? { status: "loading" as const } : null;
    function focusTrigger() {
      restoring_focus.current = true;
      trigger_ref.current?.focus({ preventScroll: true });
      restoring_focus.current = false;
    }
    function onOverlayKey(event: KeyboardEvent<HTMLDivElement>) {
      if (event.key === "ArrowLeft" || event.key === "Escape" || event.key === "Tab") {
        focusTrigger();
        if (event.key !== "Tab") event.preventDefault();
        if (event.key !== "ArrowLeft") close();
      }
    }
    return (
      <AnchoredOverlay
        aria_label="消息预览"
        bounds_ref={body_ref}
        class_name={styles.preview}
        on_request_close={close}
        open={target !== null}
        overlay_ref={overlay_ref}
        placement="right"
        region_props={{
          role: "region",
          onPointerEnter: () => { pointer_inside.current = true; clearTimeout(close_timer.current); },
          onPointerLeave: event => leave(event.clientX, event.clientY),
          onBlur: event => { if (!event.currentTarget.contains(event.relatedTarget)) scheduleClose(); },
          onKeyDown: onOverlayKey,
        }}
        trigger_ref={trigger_ref}
      >
        <div className={styles.preview_text} id={text_id}>
          <PreviewText side="user" slot={preview?.user ?? (id ? store.cachedPreview(id) : undefined) ?? { status: "loading" }} retry={() => {
            focusTrigger();
            if (id) void store.openPreview(id, "user");
          }} />
          {assistant && <PreviewText side="assistant" slot={assistant} retry={() => {
            focusTrigger();
            if (id) void store.openPreview(id, "assistant");
          }} />}
        </div>
        {location && (location.loading || location.error) && (
          <div className={styles.feedback}>
            <span role="status">{location.loading ? "正在定位…" : location.error === "这条消息已不可用"
              ? location.error : `定位失败 · ${location.error}`}</span>
            {location.error && target && <button onClick={() => { focusTrigger(); retryLocate(target); }} type="button">重试</button>}
          </div>
        )}
      </AnchoredOverlay>
    );
  }

  return { target, text_id, close, enter, leave, schedule, scheduleClose, focusControl, showFeedback, render, restoring_focus,
    hasFocus: () => Boolean(overlay_ref.current?.contains(document.activeElement)) };
}

function PreviewText(props: Readonly<{ side: "user" | "assistant"; slot: PreviewSlot; retry: () => void }>) {
  const user = props.side === "user";
  const { slot } = props;
  return (
    <div aria-label={user ? "用户输入" : "助手最后回复"} role="group">
      {slot.status === "ready" ? (
        <div className={user ? styles.input : styles.reply}>
          {slot.value.text.trim() ? `${slot.value.text}${slot.value.truncated ? "…" : ""}` : user ? "无文本输入" : "暂无正文"}
        </div>
      ) : slot.status === "loading" ? (
        <div role="status">
          <span className={styles.sr_only}>正在加载…</span>
          <div aria-hidden="true" className={styles.preview_skeleton}>
            <span />
            <span />
            {!user && <span />}
          </div>
        </div>
      ) : (
        <div className={styles.feedback}>
          <span role="status" title={slot.message}>
            {user ? "输入预览加载失败" : "回复预览加载失败"}
          </span>
          <button onClick={props.retry} type="button">重试</button>
        </div>
      )}
    </div>
  );
}
