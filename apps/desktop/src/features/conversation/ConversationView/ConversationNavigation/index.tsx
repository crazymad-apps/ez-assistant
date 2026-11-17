import { observer } from "mobx-react-lite";
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type RefObject } from "react";
import type { ConversationNavigationItem, MessageId } from "@ez-assistant/protocol";
import { Icon } from "../../../../components/Icon";
import { Tooltip } from "../../../../components/Tooltip";
import type { ConversationNavigationStore } from "../../../../stores/ConversationNavigationStore";
import { useNavigationPreview, type LocationFeedback } from "./NavigationPreview";
import { ROW_HEIGHT, useNavigationWindow } from "./useNavigationWindow";
import styles from "./index.module.scss";

const MAX_HEIGHT_RATIO = 0.6;
const navigation_size = {
  "--navigation-row-height": `${ROW_HEIGHT}px`,
  "--navigation-max-height": `${MAX_HEIGHT_RATIO * 100}%`,
} as CSSProperties;

type ConversationNavigationProps = Readonly<{
  store: ConversationNavigationStore;
  layout: HTMLElement;
  scroll_ref: RefObject<HTMLDivElement | null>;
  content_ref: RefObject<HTMLDivElement | null>;
  locate: (id: MessageId, on_error: (message: string) => void, current: () => boolean) => Promise<boolean>;
  locate_revision: number;
  cancel_location: () => void;
}>;

type FocusRequest = Readonly<{ id: MessageId; delta: number } | { latest: true }>;
type Target = Readonly<{ item: ConversationNavigationItem; button: HTMLButtonElement }>;

export const ConversationNavigation = observer(function ConversationNavigation(props: ConversationNavigationProps) {
  const { store, layout, scroll_ref, content_ref } = props;
  const nav_ref = useRef<HTMLElement>(null);
  const buttons = useRef(new Map<MessageId, HTMLButtonElement>());
  const [available_height, setAvailableHeight] = useState(0);
  const [focused, setFocused] = useState<ConversationNavigationItem | null>(null);
  const [focus_request, setFocusRequest] = useState<FocusRequest | null>(null);
  const [feedback, setFeedback] = useState<LocationFeedback | null>(null);
  const [notice, setNotice] = useState("");
  const locate_intent = useRef<object | null>(null);
  const silent_focus = useRef(false);
  const key_intent = useRef<object | null>(null);
  const retry_ref = useRef<HTMLButtonElement>(null);
  const preview = useNavigationPreview(store, scroll_ref, locate_intent);
  const windowed = useNavigationWindow(store, nav_ref, available_height);
  const { items, range, indices } = windowed;
  const focus_index = focused ? indices.get(focused.user_message_id) : undefined;
  const inView = (index: number | undefined) => index !== undefined && index >= range.first && index < range.last;
  const tab_index = inView(focus_index) ? focus_index : range.last - 1;
  const visible = available_height > 0;
  const mounted = visible && (items.length > 0 || store.loading_page || Boolean(store.page_error));

  useLayoutEffect(() => {
    const body = scroll_ref.current;
    const content = content_ref.current;
    if (!body || !content) return;
    let showing = false;
    const measure = () => {
      const box = body.getBoundingClientRect();
      const gap = content.getBoundingClientRect().left - box.left;
      showing = box.height >= 192 && gap >= (showing ? 80 : 88);
      if (!showing && (nav_ref.current?.contains(document.activeElement) || preview.hasFocus())) {
        body.focus({ preventScroll: true });
      }
      setAvailableHeight(showing ? layout.getBoundingClientRect().height * MAX_HEIGHT_RATIO : 0);
    };
    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(layout);
    observer?.observe(body);
    observer?.observe(content);
    window.addEventListener("resize", measure);
    window.visualViewport?.addEventListener("resize", measure);
    return () => {
      if (nav_ref.current?.contains(document.activeElement) || preview.hasFocus()) body.focus({ preventScroll: true });
      observer?.disconnect();
      window.removeEventListener("resize", measure);
      window.visualViewport?.removeEventListener("resize", measure);
    };
  }, [layout, scroll_ref, content_ref]);

  useLayoutEffect(() => {
    if (!visible) return;
    store.activate();
    return () => {
      store.suspend();
      preview.close();
      locate_intent.current = null;
      key_intent.current = null;
    };
  }, [store, visible, preview.close]);

  const needs_first_page = store.pages.length === 0;
  useEffect(() => {
    if (visible && needs_first_page && !store.loading_page && !store.page_error) void store.loadPage();
  }, [visible, needs_first_page, store, store.loading_page, store.page_error]);

  useLayoutEffect(() => {
    locate_intent.current = null;
    setFeedback(null);
    preview.close();
  }, [props.locate_revision, visible]);

  function focusRow(index: number, silent: boolean) {
    const item = items[index];
    const button = item && buttons.current.get(item.user_message_id);
    if (!button) return false;
    silent_focus.current = silent;
    button.focus({ preventScroll: true });
    silent_focus.current = false;
    setFocused(item);
    return true;
  }

  useLayoutEffect(() => {
    if (!mounted) return;
    if (focus_request) {
      const index = "latest" in focus_request ? items.length - 1
        : Math.max(0, Math.min(items.length - 1, (indices.get(focus_request.id) ?? 0) + focus_request.delta));
      if ("latest" in focus_request) windowed.followLatest();
      else windowed.reveal(index, focus_request.delta > 0);
      if (index < 0 || focusRow(index, false)) setFocusRequest(null);
      return;
    }
    if (focused && !inView(focus_index) && buttons.current.get(focused.user_message_id) === document.activeElement) {
      preview.close();
      const nearest = focus_index !== undefined && focus_index < range.first ? range.first : range.last - 1;
      if (!focusRow(nearest, true)) {
        scroll_ref.current?.focus({ preventScroll: true });
        setFocused(null);
      }
    }
  });

  async function moveFocus(next: FocusRequest) {
    const intent = {};
    key_intent.current = intent;
    let success = true;
    if ("latest" in next) {
      if (windowed.has_newer) success = await windowed.readBoundary("latest");
    } else {
      if (next.delta < 0) windowed.pauseFollow();
      const index = (indices.get(next.id) ?? 0) + next.delta;
      if (index < 0 && windowed.has_older) success = await windowed.readBoundary("older");
      if (index >= items.length && windowed.has_newer) success = await windowed.readBoundary("newer");
    }
    if (success && key_intent.current === intent) setFocusRequest(next);
  }

  async function locate(target: Target) {
    const id = target.item.user_message_id;
    props.cancel_location();
    const intent = {};
    locate_intent.current = intent;
    key_intent.current = null;
    const current = () => locate_intent.current === intent;
    const on_error = (error: string) => {
      if (!current()) return;
      setFeedback({ id, loading: false, error });
      preview.showFeedback(target);
    };
    preview.close();
    setNotice("");
    setFeedback({ id, loading: false });
    const timer = setTimeout(() => {
      if (!current()) return;
      setFeedback({ id, loading: true });
      preview.showFeedback(target);
    }, 150);
    try {
      if (!await store.confirmUser(id)) {
        on_error("这条消息已不可用");
      } else if (current() && await props.locate(id, on_error, current) && current()) {
        setFeedback(null);
        preview.close();
        setNotice("已定位到消息");
      } else if (current()) {
        setFeedback(previous => previous?.error ? previous : null);
      }
    } catch (error) {
      on_error(error instanceof Error ? error.message : "请重试。");
    } finally {
      clearTimeout(timer);
      if (current()) locate_intent.current = null;
    }
  }

  function handleKey(event: KeyboardEvent<HTMLButtonElement>, item: ConversationNavigationItem) {
    const id = item.user_message_id;
    const target = { item, button: event.currentTarget };
    if (event.key === "ArrowRight") {
      if (preview.target?.item.user_message_id === id && preview.focusControl()) event.preventDefault();
      return;
    }
    if (event.key === "Tab") {
      key_intent.current = null;
      preview.close();
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (!event.repeat) void locate(target);
      return;
    }
    const page = Math.max(1, Math.floor(windowed.height / ROW_HEIGHT));
    const delta = { ArrowUp: -1, ArrowDown: 1, PageUp: -page, PageDown: page }[event.key];
    if (event.key === "Home" || event.key === "End" || delta !== undefined) {
      event.preventDefault();
      const index = indices.get(id) ?? 0;
      const crosses_error = store.page_error && (event.key === "End" && store.page_direction !== "older"
        || delta !== undefined && (index + delta < 0 && store.page_direction === "older"
          || index + delta >= items.length && store.page_direction !== "older"));
      if (crosses_error) {
        preview.close();
        retry_ref.current?.focus();
      } else {
        void moveFocus(event.key === "End" ? { latest: true }
          : { id, delta: event.key === "Home" ? -index : delta! });
      }
    }
  }

  if (!mounted) return null;

  const rendered = items.slice(range.start, range.end);
  // 裁剪前保留旧焦点节点一帧，布局阶段将焦点交给相邻可见标记。
  if (focused && buttons.current.get(focused.user_message_id) === document.activeElement
    && !rendered.some(item => item.user_message_id === focused.user_message_id)) rendered.push(focused);
  const boundary = store.loading_page || store.page_error ? (
    <div className={styles.boundary} style={{ top: windowed.before ? 0 : windowed.total - ROW_HEIGHT }}>
      {store.loading_page ? <span aria-label="正在加载消息导航" className={styles.loading} role="status" />
        : <Tooltip content={`消息导航加载失败 · ${store.page_error}`}>
          <button aria-label="重试消息导航" className={styles.marker} onClick={() => {
            if (document.activeElement === retry_ref.current
              && !focusRow(tab_index ?? -1, true)) scroll_ref.current?.focus({ preventScroll: true });
            void store.retryPage();
          }} ref={retry_ref} tabIndex={items.length ? -1 : 0} type="button">
            <Icon name="refresh" size={12} />
          </button>
        </Tooltip>}
    </div>
  ) : null;

  return (
    <nav aria-label="消息导航" className={styles.navigation}
      onScroll={() => { if (windowed.onScroll()) preview.close(); }}
      onWheel={event => {
        if (!event.currentTarget.contains(event.target as Node)) return;
        preview.close();
        windowed.browse(event.deltaY);
      }}
      ref={nav_ref} style={navigation_size}>
      <div className={styles.track} style={{ height: windowed.total }}>
        {boundary}
        {rendered.map(item => {
          const id = item.user_message_id;
          const active_preview = preview.target?.item.user_message_id === id;
          const user = active_preview && store.preview?.user_message_id === id ? store.preview.user : null;
          const label = user?.status === "ready" ? Array.from(user.value.text.trim()).slice(0, 32).join("") : "";
          return <button
            aria-busy={feedback?.id === id && !feedback.error || undefined}
            aria-describedby={active_preview ? preview.text_id : undefined}
            aria-label={label ? `查看消息 · ${label}` : "查看消息"}
            className={styles.marker}
            data-navigation-message-id={id}
            key={id}
            onBlur={event => {
              if (!(event.relatedTarget instanceof HTMLElement) || !event.relatedTarget.closest('[data-overlay-region="floating"]')) {
                preview.scheduleClose();
              }
            }}
            onClick={event => { void locate({ item, button: event.currentTarget }); }}
            onFocus={event => {
              setFocused(item);
              if (!silent_focus.current && !preview.restoring_focus.current && event.currentTarget.matches(":focus-visible")) {
                preview.schedule({ item, button: event.currentTarget }, true);
              }
            }}
            onKeyDown={event => handleKey(event, item)}
            onPointerEnter={event => {
              if (event.pointerType === "mouse") preview.enter({ item, button: event.currentTarget }, event.clientX, event.clientY);
            }}
            onPointerLeave={event => { if (event.pointerType === "mouse") preview.leave(event.clientX, event.clientY); }}
            ref={node => { if (node) buttons.current.set(id, node); else buttons.current.delete(id); }}
            style={{ top: windowed.before + (indices.get(id) ?? range.first) * ROW_HEIGHT }}
            tabIndex={indices.get(id) === tab_index ? 0 : -1}
            type="button"
          >
            <span className={styles.line} />
            {feedback?.id === id && !feedback.error && <span className={styles.loading} />}
          </button>;
        })}
      </div>
      <span className={styles.sr_only} role="status">{notice}</span>
      {preview.render(feedback, target => void locate(target))}
    </nav>
  );
});
