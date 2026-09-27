import { useEffect, useRef, type ReactNode } from "react";
import styles from "./index.module.scss";

type WorkspaceTransitionProps = {
  readonly ready: boolean;
  readonly entry: ReactNode;
  readonly children: ReactNode;
};

/** 连接就绪后直接显示主界面，并将焦点交给可用的消息输入框。 */
export function WorkspaceTransition({ ready, entry, children }: WorkspaceTransitionProps) {
  const workspace = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!ready) return;
    const target = workspace.current?.querySelector<HTMLElement>('[aria-label="输入消息"]:not(:disabled)') ?? workspace.current;
    target?.focus({ preventScroll: true });
  }, [ready]);

  return (
    <div className={styles.scene} data-workspace-transition={ready ? "complete" : "waiting"}>
      {ready
        ? <div className={styles.workspace} ref={workspace} tabIndex={-1}>{children}</div>
        : <div className={styles.entry}>{entry}</div>}
    </div>
  );
}
