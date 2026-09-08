import { observer } from "mobx-react-lite";
import { useState } from "react";
import { Button } from "../../../../components/Button";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import type { DesktopCloseBehavior } from "../../../../native-bridge/desktopPreferences";
import { openRuntimeHome } from "../../../../native-bridge/runtimeHome";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SettingsPageContainer } from "../SettingsPageContainer";
import { SettingsMessages } from "../SettingsMessages";
import styles from "./index.module.scss";

const close_behavior_options: readonly SelectionOption<DesktopCloseBehavior>[] = [
  { value: "hide_to_tray", label: "隐藏到托盘", description: "关闭主窗口后保留桌面客户端" },
  { value: "quit_desktop", label: "退出客户端", description: "关闭主窗口时进入退出确认" },
];

export const LocalRuntimeSettingsPage = observer(function LocalRuntimeSettingsPage(props: Readonly<{ on_back: () => void }>) {
  const root = useRootStore();
  const lifecycle = root.desktop_lifecycle;
  const [open, setOpen] = useState(false);
  async function openHome() {
    try { await openRuntimeHome(); }
    catch (error) { root.settings.showError(error instanceof Error ? error.message : "无法打开运行时目录。"); }
  }
  return <SettingsPageContainer title="本机与客户端" on_back={props.on_back} back_label="返回 Runtime">
    <section className={styles.section}>
      <h4>本机 Runtime</h4>
      {!lifecycle.local_impact_known && <p>以下操作作用于这台电脑，当前连接的远程 Host 保持运行。</p>}
      <div className={styles.directory}><span>运行时目录</span><Button onClick={() => void openHome()} variant="text">打开目录</Button></div>
      <div className={styles.actions}>
        <Button disabled={lifecycle.pending} onClick={() => lifecycle.request("restart_runtime")}>重启本机 Runtime</Button>
        <Button disabled={lifecycle.pending} onClick={() => lifecycle.request("stop_runtime")} variant="danger">停止本机 Runtime</Button>
      </div>
    </section>
    <section className={styles.section}>
      <h4>桌面行为</h4>
      <div className={styles.behavior}><span>关闭主窗口时</span><SelectionPopover
        aria_label="关闭主窗口时" content_width="content" open={open} on_open_change={setOpen}
        on_select={(value) => lifecycle.setCloseBehavior(value)} options={close_behavior_options}
        selected={lifecycle.close_behavior} trigger_variant="field"
      /></div>
    </section>
    <SettingsMessages />
  </SettingsPageContainer>;
});
