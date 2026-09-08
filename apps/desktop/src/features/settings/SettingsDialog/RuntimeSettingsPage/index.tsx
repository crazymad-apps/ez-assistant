import { isTauri } from "@tauri-apps/api/core";
import { observer } from "mobx-react-lite";
import type { RuntimeSettingsPageId } from "../../../../stores/SettingsStore";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { RuntimeConnectionSettings } from "../../../runtime-access/RuntimeConnectionSettings";
import { HostAccessSettings } from "../../../runtime-access/HostAccessSettings";
import { RuntimeOverviewPage } from "../RuntimeOverviewPage";
import { RuntimeDiagnosticsPage } from "../RuntimeDiagnosticsPage";
import { LocalRuntimeSettingsPage } from "../LocalRuntimeSettingsPage";

/** 二级页仍使用 SettingsStore 的同一导航状态；返回也经过设置弹窗的脏表单保护。 */
export const RuntimeSettingsPage = observer(function RuntimeSettingsPage(props: Readonly<{
  onNavigate: (page: RuntimeSettingsPageId) => void;
  onDirtyChange: (dirty: boolean) => void;
}>) {
  const root = useRootStore();
  const desktop = isTauri();
  const local = desktop && root.desktop_lifecycle.local_impact_known;
  const target = local ? "这台电脑" : root.connection.address ?? "尚未连接";
  const back = () => props.onNavigate("runtime");
  switch (root.settings.page) {
    case "runtime_connection":
      if (desktop) return <RuntimeConnectionSettings on_back={back} />;
      break;
    case "host_access":
      return <HostAccessSettings on_back={back} onDirtyChange={props.onDirtyChange} />;
    case "runtime_diagnostics":
      return <RuntimeDiagnosticsPage on_back={back} />;
    case "runtime_local":
      if (desktop) return <LocalRuntimeSettingsPage on_back={back} />;
  }
  return <RuntimeOverviewPage onNavigate={props.onNavigate} target={target} local={local} desktop={desktop} />;
});
