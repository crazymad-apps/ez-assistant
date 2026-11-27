import { createContext, useCallback, useContext, useEffect, useRef, useState, type PropsWithChildren } from "react";
import { createPortal } from "react-dom";
import { Icon } from "../Icon";
import {
  Flip,
  gsap,
  MOTION_DURATION,
  MOTION_EASE,
  prefersReducedMotion,
  useGSAP,
} from "../Motion";
import styles from "./index.module.scss";

type MessageKind = "success" | "error";
type Message = Readonly<{ id: number; kind: MessageKind; text: string }>;
type ShowMessage = (text: string, kind?: MessageKind) => void;
type MessageTimers = Readonly<{ exit: number; remove: number }>;

const EXIT_DURATION_MS = Math.round(MOTION_DURATION.fast * 1000);
const AppMessageContext = createContext<ShowMessage | null>(null);

export function AppMessageProvider({ children }: PropsWithChildren) {
  const [messages, setMessages] = useState<readonly Message[]>([]);
  const next_id = useRef(0);
  const host_ref = useRef<HTMLDivElement>(null);
  const item_refs = useRef(new Map<number, HTMLDivElement>());
  const animated_ids = useRef(new Set<number>());
  const flip_state_ref = useRef<Flip.FlipState | null>(null);
  const timers = useRef(new Map<number, MessageTimers>());

  useEffect(() => () => {
    for (const timer of timers.current.values()) {
      window.clearTimeout(timer.exit);
      window.clearTimeout(timer.remove);
    }
    timers.current.clear();
  }, []);

  useGSAP(
    () => {
      const active_ids = new Set(messages.map((item) => item.id));
      for (const id of animated_ids.current) {
        if (!active_ids.has(id)) animated_ids.current.delete(id);
      }

      const flip_state = flip_state_ref.current;
      flip_state_ref.current = null;
      if (flip_state && !prefersReducedMotion()) {
        Flip.from(flip_state, {
          duration: MOTION_DURATION.medium,
          ease: MOTION_EASE.enter,
          prune: true,
          simple: true,
        });
      }

      if (prefersReducedMotion()) {
        for (const item of messages) animated_ids.current.add(item.id);
        return;
      }

      for (const item of messages) {
        if (animated_ids.current.has(item.id)) continue;
        animated_ids.current.add(item.id);
        const element = item_refs.current.get(item.id);
        if (!element) continue;
        gsap.fromTo(
          element,
          { opacity: 0, y: -8 },
          {
            clearProps: "transform,opacity",
            duration: 0.16,
            ease: MOTION_EASE.enter,
            opacity: 1,
            y: 0,
          },
        );
      }
    },
    { dependencies: [messages], scope: host_ref },
  );

  const show_message = useCallback<ShowMessage>((text, kind = "success") => {
    const id = ++next_id.current;
    const lifetime = kind === "error" ? 5000 : 3500;
    const existing = Array.from(item_refs.current.values());
    if (existing.length > 0 && !prefersReducedMotion()) {
      flip_state_ref.current = Flip.getState(existing);
    }
    setMessages((current) => [...current.slice(-2), { id, kind, text }]);

    // 在总显示时长结束前预留退场时长，保证 DOM 按既有 3500ms / 5000ms 契约准时移除。
    const exit = window.setTimeout(() => {
      const element = item_refs.current.get(id);
      if (element && !prefersReducedMotion()) {
        gsap.to(element, {
          autoAlpha: 0,
          duration: MOTION_DURATION.fast,
          ease: MOTION_EASE.exit,
          overwrite: "auto",
          y: -6,
        });
      }
    }, Math.max(0, lifetime - EXIT_DURATION_MS));

    const remove = window.setTimeout(() => {
      timers.current.delete(id);
      const remaining = Array.from(item_refs.current.entries())
        .filter(([entry_id]) => entry_id !== id)
        .map(([, element]) => element);
      if (remaining.length > 0 && !prefersReducedMotion()) {
        flip_state_ref.current = Flip.getState(remaining);
      }
      setMessages((current) => current.filter((item) => item.id !== id));
    }, lifetime);

    timers.current.set(id, { exit, remove });
  }, []);

  return <AppMessageContext.Provider value={show_message}>
    {children}
    {createPortal(<div className={styles.host} ref={host_ref}>
      {messages.map((item) => <div
        className={styles.bubble}
        data-kind={item.kind}
        key={item.id}
        ref={(node) => {
          if (node) item_refs.current.set(item.id, node);
          else item_refs.current.delete(item.id);
        }}
        role={item.kind === "error" ? "alert" : "status"}
      >
        <Icon name={item.kind === "error" ? "x" : "check"} size={16} />
        <span>{item.text}</span>
      </div>)}
    </div>, document.body)}
  </AppMessageContext.Provider>;
}

export function useAppMessage(): ShowMessage {
  const show_message = useContext(AppMessageContext);
  if (!show_message) throw new Error("AppMessageProvider is missing");
  return show_message;
}
