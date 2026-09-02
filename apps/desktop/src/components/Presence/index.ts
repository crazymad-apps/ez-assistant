import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type TransitionEvent,
} from "react";

export type PresenceState = "entering" | "entered" | "exiting";

export type Presence = Readonly<{
  mounted: boolean;
  state: PresenceState;
  onTransitionEnd: (event: TransitionEvent<HTMLElement>) => void;
}>;

const PresenceContext = createContext<Presence | null>(null);

export function PresenceBoundary(props: Readonly<{
  children: ReactNode;
  exit_duration_ms?: number;
  present: boolean;
}>) {
  const presence = usePresence(props.present, props.exit_duration_ms ?? 120);
  const retained_children_ref = useRef(props.children);
  if (props.present) retained_children_ref.current = props.children;
  if (!presence.mounted) return null;
  return createElement(
    PresenceContext.Provider,
    { value: presence },
    props.present ? props.children : retained_children_ref.current,
  );
}

export function usePresenceBoundary(): Presence | null {
  return useContext(PresenceContext);
}

/**
 * 让视觉退出和业务 open 状态解耦；timer 是 transitionend 未到达时的必达清理路径。
 */
export function usePresence(present: boolean, exit_duration_ms: number): Presence {
  const [state, setState] = useState<PresenceState | null>(present ? "entering" : null);
  const state_ref = useRef(state);
  state_ref.current = state;

  useLayoutEffect(() => {
    let frame = 0;
    let fallback = 0;
    if (present) {
      setState("entering");
      frame = requestAnimationFrame(() => setState("entered"));
    } else if (state_ref.current !== null) {
      setState("exiting");
      const reduced_motion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
      fallback = window.setTimeout(
        () => setState((current) => current === "exiting" ? null : current),
        reduced_motion ? 40 : exit_duration_ms + 40,
      );
    }
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(fallback);
    };
  }, [exit_duration_ms, present]);

  const onTransitionEnd = useCallback((event: TransitionEvent<HTMLElement>) => {
    if (event.target !== event.currentTarget || state_ref.current !== "exiting") return;
    setState(null);
  }, []);

  return {
    mounted: state !== null,
    state: state ?? "exiting",
    onTransitionEnd,
  };
}
