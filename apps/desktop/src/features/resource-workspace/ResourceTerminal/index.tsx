import { useEffect, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import type { TerminalController } from "../TerminalController";
import styles from "./index.module.scss";
import { SelectionPopover } from "../../../components/SelectionPopover";
import { shellLabels } from "../../../runtime-client/shellPresentation";
import type { ShellKind } from "@ez-assistant/protocol";

export const ResourceTerminal = observer(function ResourceTerminal(props: Readonly<{ controller: TerminalController; active: boolean }>) {
  const { controller, active } = props;
  const viewport = useRef<HTMLDivElement>(null);
  const [shell_open, setShellOpen] = useState(false);
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
    <div className={styles.toolbar} role="toolbar" aria-label="终端工具栏">
      <SelectionPopover<ShellKind | "">
        aria_label="终端 Shell 类型" trigger_variant="compact" open={shell_open} on_open_change={setShellOpen}
        selected={controller.shell_kind ?? ""} placeholder="系统默认 Shell"
        disabled={!["running", "error", "exited"].includes(controller.status)}
        title="切换 Shell 会结束当前终端进程并重新打开"
        options={controller.shell_catalog.map((entry) => ({
          value: entry.kind, label: `${shellLabels[entry.kind]}${entry.available ? "" : "（未安装）"}`,
          disabled: !entry.available,
          description: entry.available ? undefined : entry.kind === "powershell_7" ? "安装 PowerShell 7 后可用" : entry.kind === "git_bash" ? "安装 Git for Windows 后可用" : entry.reason ?? "当前不可用",
        }))}
        on_select={(shell) => { if (shell) controller.selectShell(shell); }}
      />
    </div>
    {(controller.error || controller.status === "starting" || controller.status === "closing") &&
      <div className={styles.notice} role={controller.error ? "alert" : "status"}>
        <span>{controller.error ?? (controller.status === "closing" ? "正在结束终端…" : "正在启动终端…")}</span>
        {controller.status === "error" &&
          <Button size="small" variant="text" onClick={() => controller.restart()}>重新启动</Button>}
      </div>}
    <div className={styles.viewport} ref={viewport} />
  </div>;
});
