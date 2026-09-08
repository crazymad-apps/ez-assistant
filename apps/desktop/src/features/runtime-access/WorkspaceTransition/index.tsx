import { useEffect, useRef, useState, type ReactNode } from "react";
import styles from "./index.module.scss";

type WorkspaceTransitionProps = {
  readonly ready: boolean;
  readonly connected: boolean;
  readonly entry: ReactNode;
  readonly children: ReactNode;
};

/** Desktop / Web 共用入口转场；真实连接状态先就绪，动画不承载连接或业务状态。 */
export function WorkspaceTransition({ ready, connected, entry, children }: WorkspaceTransitionProps) {
  const [settled, setSettled] = useState(false);
  const scene = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const workspace = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!ready || settled) return;
    const root = scene.current;
    const surface = canvas.current;
    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    const abort = new AbortController();
    let disposed = false;
    let loadingDeadline = 0;
    const finish = () => {
      if (disposed) return;
      disposed = true;
      clearTimeout(loadingDeadline);
      abort.abort();
      setSettled(true);
    };
    if (!connected || reduced.matches || document.hidden || !root || !surface) { finish(); return; }
    const visibility = () => { if (document.hidden) finish(); };
    const motion = () => { if (reduced.matches) finish(); };
    document.addEventListener("visibilitychange", visibility);
    reduced.addEventListener("change", motion);
    loadingDeadline = window.setTimeout(finish, 1200);
    void import("./rocketPlayback").then(({ playRocket }) => {
      if (disposed) return;
      return playRocket(surface, abort.signal, (frame) => {
        clearTimeout(loadingDeadline);
        const fade = Math.max(0, Math.min(1, (frame - 16) / 32));
        const reveal = Math.max(0, Math.min(1, (frame - 68) / 32));
        root.dataset.launchFrame = String(frame);
        root.style.setProperty("--entry-opacity", String(1 - fade));
        root.style.setProperty("--entry-offset", `${-48 * fade}px`);
        root.style.setProperty("--background-opacity", String(1 - reveal));
        root.style.setProperty("--workspace-opacity", String(reveal));
        root.style.setProperty("--workspace-offset", `${28 * (1 - reveal)}px`);
      });
    }).catch(() => { /* 媒体不可用时按设计直接进入工作台，不改变连接成功事实。 */ }).finally(finish);
    return () => {
      disposed = true;
      clearTimeout(loadingDeadline);
      abort.abort();
      document.removeEventListener("visibilitychange", visibility);
      reduced.removeEventListener("change", motion);
    };
  }, [ready, connected, settled]);

  useEffect(() => {
    if (!settled) return;
    const target = workspace.current?.querySelector<HTMLElement>('[aria-label="输入消息"]:not(:disabled)') ?? workspace.current;
    target?.focus({ preventScroll: true });
  }, [settled]);

  return (
    <div className={styles.scene} ref={scene} data-workspace-transition={settled ? "complete" : ready ? "playing" : "waiting"}>
      <div className={styles.workspace} ref={workspace} inert={!settled} aria-hidden={!settled} tabIndex={-1}>
        {(ready || settled) && children}
      </div>
      {!settled && <div className={styles.entry} inert={ready} aria-hidden={ready}>{entry}</div>}
      {ready && !settled && <canvas className={styles.rocket} ref={canvas} width={1024} height={640} aria-hidden="true" />}
    </div>
  );
}
