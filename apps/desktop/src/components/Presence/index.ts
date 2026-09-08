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
 * animate_initial 仅控制挂载时已显示的内容；之后 present 切换仍正常过渡。
 */
export function usePresence(
  present: boolean,
  exit_duration_ms: number,
  options: Readonly<{ animate_initial?: boolean }> = {},
): Presence {
  const [state, setState] = useState<PresenceState | null>(() => {
    if (!present) return null;
    return options.animate_initial === false ? "entered" : "entering";
  });
  const state_ref = useRef(state);
  state_ref.current = state;

  useLayoutEffect(() => {
    let frame = 0;
    let fallback = 0;
    if (present) {
      // 默认展开直接呈现最终状态，StrictMode 重放 effect 也不补播入场动画。
      if (state_ref.current === "entered") return;
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
