import { useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import type { AgentShellSettings, SessionViewSnapshot, ShellKind } from "@ez-assistant/protocol";
import { SelectionPopover } from "../../../components/SelectionPopover";
import { Button } from "../../../components/Button";
import { useRootStore } from "../../../stores/RootStoreContext";
import { shellLabels } from "../../../runtime-client/shellPresentation";
import styles from "./index.module.scss";

export const SessionShellSetting = observer(function SessionShellSetting(props: Readonly<{ view: SessionViewSnapshot }>) {
  const root = useRootStore();
  const [open, setOpen] = useState(false);
  const [catalog, setCatalog] = useState<AgentShellSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const session_id = props.view.session.session_id;
  const connection_state = root.connection.state;
  const address = root.connection.address;
  useEffect(() => {
    let current = true;
    setCatalog(null);
    setError(null);
    void root.getAgentShellSettings().then((value) => { if (current) setCatalog(value); })
      .catch(() => { if (current) setError("无法读取 Shell 目录，请重新打开切换列表。"); });
    return () => { current = false; };
  }, [root, session_id, connection_state, address, open]);
  const selected = props.view.agent_shell_kind;
  const selected_entry = catalog?.catalog.find((entry) => entry.kind === selected);
  const unavailable = Boolean(selected) && catalog !== null && !selected_entry?.available;
  const unix = catalog?.catalog.some((entry) => entry.kind === "posix_sh" && entry.available);
  const pending = props.view.queue.items.flatMap((entry) => {
    if (entry.type === "command" && entry.payload.command.type === "agent_shell_switch") return [entry.payload.command.payload.shell];
    return [];
  });
  const label = selected ? shellLabels[selected] : unix ? "/bin/sh" : "尚未绑定";
  return <section className={styles.shell_setting} aria-label="Agent Shell">
    <div className={styles.shell_actions}>
    <SelectionPopover<ShellKind | "">
      aria_label="切换会话 Agent Shell"
      trigger_variant="compact"
      trigger_content={label}
      selected={pending.length > 0 ? "" : selected ?? ""}
      open={open}
      on_open_change={setOpen}
      disabled={Boolean(unix) || root.pending_session_action || props.view.session.lifecycle === "archived"}
      options={(catalog?.catalog ?? []).filter((entry) => entry.kind !== "posix_sh").map((entry) => ({
        value: entry.kind, label: `${shellLabels[entry.kind]}${entry.available ? "" : "（未安装）"}`,
        disabled: !entry.available, description: entry.available ? undefined : entry.reason ?? "当前不可用",
      }))}
      on_select={(shell) => { if (shell) void root.submitSessionCommand(session_id, { type: "agent_shell_switch", payload: { shell } }); }}
    />
    {selected && !unix && <Button size="small" variant="text" aria-label="刷新 Shell 环境"
      disabled={root.pending_session_action || pending.length > 0 || props.view.session.lifecycle === "archived"}
      onClick={() => { void root.submitSessionCommand(session_id, { type: "agent_shell_switch", payload: { shell: selected } }); }}
    >刷新</Button>}
    </div>
    {unavailable && <small role="status" data-shell-unavailable="true">
      当前绑定不可用：{selected_entry?.reason ?? "当前 Host 无法执行该 Shell"}。请恢复安装或切换 Shell。
    </small>}
    {pending.map((shell, index) => <small key={`${index}:${shell}`}>切换排队中 → {shellLabels[shell]}</small>)}
    {error && <small role="status">{error}</small>}
  </section>;
});
