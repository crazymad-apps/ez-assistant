import { useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import type { ShellKind } from "@ez-assistant/protocol";
import { SelectionPopover } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import styles from "./index.module.scss";
import { shellLabels as labels } from "../../../../runtime-client/shellPresentation";

export const AgentShellSetting = observer(function AgentShellSetting() {
  const root = useRootStore();
  const settings = root.settings;
  const [open, setOpen] = useState(false);
  const connection_state = root.connection.state;
  const address = root.connection.address;
  useEffect(() => {
    setOpen(false);
    void settings.loadAgentShellSettings();
  }, [settings, connection_state, address]);
  const snapshot = settings.agent_shell_settings;
  const unix = snapshot?.catalog.some((entry) => entry.kind === "posix_sh" && entry.available) ?? false;
  const selected = snapshot?.default_agent_shell ?? (unix ? "posix_sh" : "windows_powershell_51");
  const available = snapshot?.catalog.some((entry) => entry.kind === selected && entry.available);
  return <section className={styles.shell_setting} aria-label="默认 Agent Shell">
    <strong>默认 Agent Shell</strong>
    <SelectionPopover<ShellKind>
      aria_label="默认 Agent Shell"
      trigger_variant="field"
      trigger_class_name={styles.shell_selector}
      open={open}
      on_open_change={setOpen}
      disabled={!snapshot || settings.shell_loading || settings.pending_action !== null || unix}
      selected={selected}
      trigger_content={!snapshot ? (settings.shell_loading ? "正在读取…" : "尚未读取") : `${labels[selected]}${available ? "" : "（不可用）"}`}
      options={(snapshot?.catalog ?? []).filter((entry) => unix ? entry.kind === "posix_sh" : entry.kind !== "posix_sh").map((entry) => ({
        value: entry.kind,
        label: `${labels[entry.kind]}${entry.available ? "" : "（未安装）"}`,
        disabled: !entry.available,
        description: entry.available ? undefined : entry.reason ?? "当前不可用",
      }))}
      on_select={(shell) => { void settings.setDefaultAgentShell(shell); }}
    />
  </section>;
});
