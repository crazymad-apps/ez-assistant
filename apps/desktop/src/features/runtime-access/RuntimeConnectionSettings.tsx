import { observer } from "mobx-react-lite";
import { useApplicationConnection } from "./ApplicationConnectionContext";
import { RuntimeConnectionForm } from "./RuntimeConnectionForm";
import { SettingsPageContainer } from "../settings/SettingsDialog/SettingsPageContainer";

export const RuntimeConnectionSettings = observer(function RuntimeConnectionSettings({ on_back }: Readonly<{ on_back: () => void }>) {
  const connection = useApplicationConnection();
  if (!connection) return null;
  return <SettingsPageContainer title="切换 Runtime" on_back={on_back} back_label="返回 Runtime">
    <RuntimeConnectionForm connection={connection} settings />
  </SettingsPageContainer>;
});
