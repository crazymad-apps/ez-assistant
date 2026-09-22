import { useCallback, useLayoutEffect, useRef, type RefObject } from "react";

import type { ConversationReadingPosition as ReadingPosition } from "../../../stores/NavigationStore";

/** 消息列表与思考区域共用事件规则；意图由用户操作改变，布局只执行该意图。 */
export function useOutputFollow(options: {
  scroll: RefObject<HTMLDivElement | null>;
  content?: RefObject<HTMLDivElement | null>;
  identity: string;
  active: boolean;
  restore: () => ReadingPosition | undefined;
  save?: (position: ReadingPosition) => void;
  recoverAnchor?: (message_id: string) => Promise<boolean>;
}) {
  const callbacks = useRef(options);
  callbacks.current = options;
  const following = useRef(true);
  const position = useRef<ReadingPosition>({ following: true, top: 0 });
  const expected_top = useRef<number | null>(null);
  const user_scroll = useRef(false);
  const recovery = useRef<{ identity: string; message_id: string } | null>(null);

  const remember = useCallback(() => {
    const node = callbacks.current.scroll.current;
    if (!node) return;
    if (recovery.current) return;
    position.current = readPosition(node, following.current);
    callbacks.current.save?.(position.current);
  }, []);

  const setTop = useCallback((node: HTMLDivElement, top: number) => {
    node.scrollTop = top;
    expected_top.current = node.scrollTop;
  }, []);

  const align = useCallback(() => {
    const node = callbacks.current.scroll.current;
    if (!node || !callbacks.current.active) return;
    if (following.current) {
      setTop(node, node.scrollHeight);
    } else if (position.current.anchor) {
      const anchor = findAnchor(node, position.current.anchor.message_id);
      if (anchor) {
        setTop(node, node.scrollTop + anchor.getBoundingClientRect().top
          - node.getBoundingClientRect().top - position.current.anchor.offset);
      } else if (callbacks.current.recoverAnchor) {
        if (!recovery.current) {
          const pending = { identity: callbacks.current.identity, message_id: position.current.anchor.message_id };
          recovery.current = pending;
          void callbacks.current.recoverAnchor(pending.message_id).then((loaded) => {
            if (recovery.current !== pending || callbacks.current.identity !== pending.identity) return;
            requestAnimationFrame(() => {
              if (recovery.current !== pending || callbacks.current.identity !== pending.identity) return;
              recovery.current = null;
              if (!loaded || !findAnchor(node, pending.message_id)) {
                position.current = { following: false, top: node.scrollTop };
              }
              align();
            });
          }).catch(() => {
            if (recovery.current !== pending) return;
            recovery.current = null;
            position.current = { following: false, top: node.scrollTop };
            remember();
          });
        }
        return;
      }
    }
    remember();
  }, [remember, setTop]);

  const pause = useCallback(() => {
    following.current = false;
    remember();
  }, [remember]);

  const bottom = useCallback(() => {
    following.current = true;
    recovery.current = null;
    user_scroll.current = false;
    align();
  }, [align]);

  useLayoutEffect(() => {
    const node = options.scroll.current;
    if (!node || !options.active) return;
    recovery.current = null;
    const restored = callbacks.current.restore();
    following.current = restored?.following ?? true;
    position.current = restored ?? { following: true, top: 0 };
    user_scroll.current = false;
    setTop(node, restored?.top ?? node.scrollHeight);
    align();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let touch_y: number | null = null;
    const markUser = (up: boolean) => {
      if (recovery.current) {
        recovery.current = null;
        position.current = readPosition(node, false);
      }
      user_scroll.current = true;
      expected_top.current = null;
      if (up) following.current = false;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => { user_scroll.current = false; }, 250);
    };
    const owns = (target: EventTarget | null, delta: number) => {
      const nested = target instanceof Element ? target.closest<HTMLElement>("[data-output-scroll]") : null;
      if (!nested || nested === node) return true;
      // 内部还有空间时由内部消费；实际到边界后允许浏览器滚动外层。
      return delta < 0 ? nested.scrollTop <= 0
        : nested.scrollTop + nested.clientHeight >= nested.scrollHeight - 1;
    };
    const wheel = (event: WheelEvent) => {
      if (event.deltaY && owns(event.target, event.deltaY)) markUser(event.deltaY < 0);
    };
    const touchStart = (event: TouchEvent) => { touch_y = event.touches[0]?.clientY ?? null; };
    const touchMove = (event: TouchEvent) => {
      const y = event.touches[0]?.clientY;
      if (y === undefined || touch_y === null) return;
      const delta = touch_y - y;
      if (owns(event.target, delta)) markUser(delta < 0);
      touch_y = y;
    };
    const key = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (event.target instanceof Element && event.target.closest("input,textarea,[contenteditable=true]")) return;
      // End 表达的是回到底部；原生平滑滚动的旧目标可能被持续输出甩开。
      if (event.key === "End" && owns(event.target, 1)) {
        event.preventDefault();
        bottom();
        return;
      }
      const up = ["ArrowUp", "PageUp", "Home"].includes(event.key) || (event.key === " " && event.shiftKey);
      const down = ["ArrowDown", "PageDown", "End", " "].includes(event.key);
      if ((up || down) && owns(event.target, up ? -1 : 1)) markUser(up);
    };
    const pointer = (event: PointerEvent) => {
      if (event.target === node) markUser(false);
    };
    const scroll = () => {
      const programmatic = expected_top.current !== null && Math.abs(node.scrollTop - expected_top.current) < 1;
      if (user_scroll.current && !programmatic) {
        following.current = node.scrollHeight - node.scrollTop - node.clientHeight <= 24;
        markUser(!following.current);
      }
      expected_top.current = null;
      if (user_scroll.current || programmatic) remember();
      else align();
    };
    node.dataset.outputScroll = "true";
    node.addEventListener("wheel", wheel, { passive: true });
    node.addEventListener("touchstart", touchStart, { passive: true });
    node.addEventListener("touchmove", touchMove, { passive: true });
    node.addEventListener("keydown", key);
    node.addEventListener("pointerdown", pointer);
    node.addEventListener("scroll", scroll);
    const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(align);
    resize?.observe(node);
    const content = options.content?.current ?? node.firstElementChild;
    if (content) resize?.observe(content);
    return () => {
      recovery.current = null;
      if (timer) clearTimeout(timer);
      resize?.disconnect();
      node.removeEventListener("wheel", wheel);
      node.removeEventListener("touchstart", touchStart);
      node.removeEventListener("touchmove", touchMove);
      node.removeEventListener("keydown", key);
      node.removeEventListener("pointerdown", pointer);
      node.removeEventListener("scroll", scroll);
    };
  }, [options.identity, options.active, options.scroll, options.content, align, bottom, remember, setTop]);

  // React 已提交新布局；沿用更新前的锚点，不能先按新尺寸覆盖阅读意图。
  useLayoutEffect(align);
  return { following, pause, bottom, remember };
}

function findAnchor(node: HTMLElement, message_id: string): HTMLElement | undefined {
  return [...node.querySelectorAll<HTMLElement>("[data-message-id]")]
    .find((item) => item.dataset.messageId === message_id);
}

function readPosition(node: HTMLElement, following: boolean): ReadingPosition {
  const top = node.getBoundingClientRect().top;
  const visible = [...node.querySelectorAll<HTMLElement>("[data-message-id]")]
    .find((item) => item.getBoundingClientRect().bottom > top + 1);
  return {
    following,
    top: node.scrollTop,
    ...(visible?.dataset.messageId ? { anchor: {
      message_id: visible.dataset.messageId,
      offset: visible.getBoundingClientRect().top - top,
    } } : {}),
  };
}
