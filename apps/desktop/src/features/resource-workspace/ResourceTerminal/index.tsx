import { useEffect, useRef } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import type { TerminalController } from "../TerminalController";
import styles from "./index.module.scss";

export const ResourceTerminal = observer(function ResourceTerminal(props: Readonly<{ controller: TerminalController; active: boolean }>) {
  const { controller, active } = props;
  const viewport = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const element = viewport.current;
    if (!element) return;
    controller.mount(element);
    const observer = new ResizeObserver(() => controller.fit());
    observer.observe(element);
    return () => { observer.disconnect(); controller.unmount(); };
  }, [controller]);
  useEffect(() => {
    if (active) controller.start();
    if (active && controller.ready && controller.status === "running") { controller.fit(); controller.focus(); }
  }, [controller, controller.ready, controller.status, active]);

  return <div className={styles.terminal}>
    <div className={styles.viewport} ref={viewport} />
    {(controller.error || controller.status === "starting" || controller.status === "closing") &&
      <div className={styles.notice} role={controller.error ? "alert" : "status"}>
        <span>{controller.error ?? (controller.status === "closing" ? "正在结束终端…" : "正在启动终端…")}</span>
        {controller.status === "error" &&
          <Button size="small" variant="text" onClick={() => controller.restart()}>重新启动</Button>}
      </div>}
  </div>;
});
