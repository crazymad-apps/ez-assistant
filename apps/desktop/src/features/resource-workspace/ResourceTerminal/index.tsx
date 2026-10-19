import { useEffect, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import type { TerminalController } from "../TerminalController";
import styles from "./index.module.scss";
import { SelectionPopover } from "../../../components/SelectionPopover";
import { shellLabels } from "../../../runtime-client/shellPresentation";
import type { ShellKind, UserTerminalShellEntry } from "@ez-assistant/protocol";
import { Dialog } from "../../../components/Dialog";
import dialogStyles from "./CloseTerminalDialog.module.scss";

function shellLabel(entry: UserTerminalShellEntry): string {
  return entry.shell === null ? `系统默认 Shell（${entry.name}）` : shellLabels[entry.shell];
}

export const ResourceTerminal = observer(function ResourceTerminal(props: Readonly<{ controller: TerminalController; active: boolean }>) {
  const { controller, active } = props;
  const viewport = useRef<HTMLDivElement>(null);
  const [shell_open, setShellOpen] = useState(false);
  const [switch_target, setSwitchTarget] = useState<UserTerminalShellEntry | null>(null);
  useEffect(() => { setSwitchTarget(null); setShellOpen(false); }, [controller, active]);
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
      <SelectionPopover<ShellKind | "default">
        aria_label="终端 Shell 类型" trigger_variant="compact" open={shell_open} on_open_change={setShellOpen}
        selected={controller.selected_shell ?? "default"}
        placeholder={controller.shell_name ? shellLabel({ shell: controller.selected_shell, name: controller.shell_name }) : "系统默认 Shell"}
        disabled={!["running", "error", "exited"].includes(controller.status)}
        title="切换 Shell 会结束当前终端进程并重新打开"
        options={controller.shell_catalog.map((entry) => ({
          value: entry.shell ?? "default",
          label: shellLabel({ ...entry, name: entry.shell === controller.selected_shell ? controller.shell_name ?? entry.name : entry.name }),
        }))}
        on_select={(value) => {
          const shell = value === "default" ? null : value;
          if (controller.status === "running" && shell === controller.selected_shell) return;
          const entry = controller.shell_catalog.find((entry) => entry.shell === shell);
          if (!entry) return;
          if (controller.terminal_id && controller.status !== "exited") setSwitchTarget(entry);
          else controller.selectShell(shell);
        }}
      />
    </div>
    {(controller.error || controller.status === "starting" || controller.status === "closing") &&
      <div className={styles.notice} role={controller.error ? "alert" : "status"}>
        <span>{controller.error ?? (controller.status === "closing" ? "正在结束终端…" : "正在启动终端…")}</span>
        {controller.status === "error" &&
          <Button size="small" variant="text" onClick={() => controller.restart()}>重新启动</Button>}
      </div>}
    {controller.status === "exited" && <div className={styles.notice} role="status">
      <span>终端已结束</span><Button size="small" variant="text" onClick={() => controller.restart()}>重新打开</Button>
    </div>}
    <div className={styles.viewport} ref={viewport} />
    {switch_target && <Dialog aria_label="切换终端 Shell" backdrop_class_name={dialogStyles.backdrop}
      dialog_class_name={dialogStyles.dialog} on_close={() => setSwitchTarget(null)}>
      <h2>切换到 {shellLabel(switch_target)}？</h2>
      <p>切换将结束当前终端中的 Shell 和前台任务，并打开新终端。</p>
      <footer>
        <Button onClick={() => setSwitchTarget(null)}>取消</Button>
        <Button variant="danger" onClick={() => { controller.selectShell(switch_target.shell); setSwitchTarget(null); }}>切换 Shell</Button>
      </footer>
    </Dialog>}
  </div>;
});
