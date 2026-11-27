import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import {
  gsap,
  MOTION_DURATION,
  MOTION_EASE,
  prefersReducedMotion,
  useGSAP,
} from "../../../components/Motion";
import styles from "./index.module.scss";

type WorkspaceTransitionProps = {
  readonly ready: boolean;
  readonly entry: ReactNode;
  readonly children: ReactNode;
};

/**
 * 首次从入口页进入主工作台时播放轻量淡入转场；转场期间工作台保持 inert，
 * 结束后卸载入口层并将焦点交给可用消息输入框。减少动态效果或页面不可见时直达工作台。
 */
export function WorkspaceTransition({ ready, entry, children }: WorkspaceTransitionProps) {
  const scene_ref = useRef<HTMLDivElement>(null);
  const workspace_ref = useRef<HTMLDivElement>(null);
  const exiting_entry_ref = useRef<HTMLDivElement>(null);
  const previous_ready_ref = useRef(ready);
  const previous_entry_ref = useRef<ReactNode>(entry);
  const [exiting_entry, setExitingEntry] = useState<ReactNode | null>(null);

  if (!ready) {
    previous_entry_ref.current = entry;
  }

  useLayoutEffect(() => {
    const was_ready = previous_ready_ref.current;
    previous_ready_ref.current = ready;
    if (!ready) {
      setExitingEntry(null);
      return;
    }
    if (!was_ready && !prefersReducedMotion() && document.visibilityState !== "hidden") {
      setExitingEntry(previous_entry_ref.current);
    }
  }, [ready]);

  const transitioning = ready && exiting_entry !== null;

  useGSAP(
    () => {
      if (!transitioning) return;
      const workspace = workspace_ref.current;
      const entry_layer = exiting_entry_ref.current;
      if (!workspace || !entry_layer) {
        setExitingEntry(null);
        return;
      }

      const finish = () => {
        setExitingEntry(null);
      };

      const timeline = gsap.timeline({ onComplete: finish });
      timeline
        .to(entry_layer, {
          autoAlpha: 0,
          duration: MOTION_DURATION.medium,
          ease: MOTION_EASE.exit,
          y: -12,
        })
        .fromTo(
          workspace,
          { opacity: 0, scale: 0.992 },
          {
            clearProps: "transform,opacity",
            duration: MOTION_DURATION.entrance,
            ease: MOTION_EASE.enter,
            opacity: 1,
            scale: 1,
          },
          "<0.06",
        );

      const handleVisibilityChange = () => {
        if (document.visibilityState === "hidden") {
          timeline.kill();
          gsap.set(workspace, { clearProps: "transform,opacity" });
          finish();
        }
      };

      document.addEventListener("visibilitychange", handleVisibilityChange);
      return () => {
        document.removeEventListener("visibilitychange", handleVisibilityChange);
        timeline.kill();
      };
    },
    { dependencies: [transitioning], scope: scene_ref },
  );

  useEffect(() => {
    if (!ready || transitioning) return;
    const target = workspace_ref.current?.querySelector<HTMLElement>('[aria-label="输入消息"]:not(:disabled)') ?? workspace_ref.current;
    target?.focus({ preventScroll: true });
  }, [ready, transitioning]);

  let state: "waiting" | "transitioning" | "complete" = "waiting";
  if (ready) {
    state = transitioning ? "transitioning" : "complete";
  }

  return (
    <div className={styles.scene} data-workspace-transition={state} ref={scene_ref}>
      {ready ? (
        <>
          <div className={styles.workspace} inert={transitioning || undefined} ref={workspace_ref} tabIndex={-1}>{children}</div>
          {transitioning && (
            <div aria-hidden="true" className={styles.entry_exiting} inert ref={exiting_entry_ref}>
              {exiting_entry}
            </div>
          )}
        </>
      ) : (
        <div className={styles.entry}>{entry}</div>
      )}
    </div>
  );
}
