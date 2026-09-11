import { Button } from "../../../components/Button";
import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useRef, useState } from "react";
import type { ModelSelection, ProviderInstanceId } from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { ModelFixedConfigPage } from "./ModelFixedConfigPage";
import { ManualModelPage } from "./ManualModelPage";
import { ModelSelectionControl } from "./ModelSelectionControl";
import { ProviderConnectionForm } from "./ProviderConnectionForm";
import { ProviderSettingsPage } from "./ProviderSettingsPage";
import { SettingsMessages } from "./SettingsMessages";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./index.module.scss";

type ModelPage = { type: "home" } | { type: "create" }
  | { type: "provider" | "edit"; provider_id: ProviderInstanceId }
  | { type: "model"; selection: ModelSelection }
  | { type: "add_model"; provider_id: ProviderInstanceId };

export const ModelsSettingsPage = observer(function ModelsSettingsPage(props: Readonly<{
  onDirtyChange: (dirty: boolean) => void;
}>) {
  const settings = useRootStore().settings;
  const [page, setPage] = useState<ModelPage>({ type: "home" });
  const [dirty, setDirty] = useState(false);
  const [pending_page, setPendingPage] = useState<ModelPage | null>(null);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const onDirtyChange = useCallback((value: boolean) => {
    setDirty(value); props.onDirtyChange(value);
  }, [props.onDirtyChange]);
  function navigate(next: ModelPage) {
    if (settings.pending_action) return;
    if (dirty) { setPendingPage(next); return; }
    settings.clearMessages(); setPage(next);
  }
  const home = () => navigate({ type: "home" });
  async function deleteProvider(id: ProviderInstanceId) {
    // 提交会先移除 Provider 投影并卸载详情页；完成导航由仍持有 page 的父级负责。
    const deleted = await settings.deleteProvider(id);
    if (deleted && mounted.current) setPage((current) => (
      current.type === "provider" && current.provider_id === id ? { type: "home" } : current
    ));
  }
  const provider_id = page.type === "model" ? page.selection.provider_instance_id : undefined;
  const provider = settings.providers.find((item) => item.provider_instance_id === (
    page.type === "provider" || page.type === "edit" || page.type === "add_model" ? page.provider_id : provider_id
  ));
  let content;
  if (page.type === "create" || (page.type === "edit" && provider)) {
    content = <ProviderConnectionForm key={provider?.provider_instance_id ?? "new"} provider={provider ?? null}
      onDirtyChange={onDirtyChange} onBack={() => provider ? navigate({ type: "provider", provider_id: provider.provider_instance_id }) : home()}
      onSaved={(saved) => { onDirtyChange(false); setPage({ type: "provider", provider_id: saved.provider_instance_id }); }} />;
  } else if (page.type === "provider" && provider) {
    content = <ProviderSettingsPage key={provider.provider_instance_id} provider={provider} onBack={home} onDelete={() => { void deleteProvider(provider.provider_instance_id); }}
      onEdit={() => navigate({ type: "edit", provider_id: provider.provider_instance_id })}
      onAddModel={() => navigate({ type: "add_model", provider_id: provider.provider_instance_id })}
      onModel={(selection) => navigate({ type: "model", selection })} />;
  } else if (page.type === "add_model" && provider) {
    content = <ManualModelPage provider={provider} onDirtyChange={onDirtyChange}
      onDeleted={() => { onDirtyChange(false); setPage({ type: "provider", provider_id: provider.provider_instance_id }); }}
      onBack={() => navigate({ type: "provider", provider_id: provider.provider_instance_id })} />;
  } else if (page.type === "model" && provider) {
    content = <ModelFixedConfigPage key={JSON.stringify(page.selection)} provider={provider} selection={page.selection}
      onDeleted={() => { onDirtyChange(false); setPage({ type: "provider", provider_id: provider.provider_instance_id }); }}
      onDirtyChange={onDirtyChange} onBack={() => navigate({ type: "provider", provider_id: provider.provider_instance_id })} />;
  } else if (page.type !== "home") {
    content = <SettingsPageContainer title="服务商不可用" on_back={home}><p>服务商已删除，请返回重新选择。</p></SettingsPageContainer>;
  } else {
    content = <SettingsPageContainer title="模型与服务商" actions={<Button variant="primary" onClick={() => navigate({ type: "create" })}>添加服务商</Button>}>
      <ModelSelectionControl purpose="default" /><ModelSelectionControl purpose="vision" />
      <div className={styles.model_rows}>{settings.providers.map((item) => <button type="button" aria-label={`管理服务商 ${item.connection.display_name} ${item.connection.endpoint}`} key={item.provider_instance_id} onClick={() => navigate({ type: "provider", provider_id: item.provider_instance_id })}>
        <span><strong>{item.connection.display_name}</strong><small>{item.connection.endpoint}</small></span><span aria-hidden="true">›</span>
      </button>)}</div>
      {!settings.loading && settings.providers.length === 0 && <p className={styles.empty_models}>添加服务商后即可获取在线模型。</p>}
      <SettingsMessages />
    </SettingsPageContainer>;
  }
  return <>{content}{pending_page && <SessionActionDialog title="放弃未保存的修改？" confirm_label="放弃修改" is_danger
    is_pending={false} on_cancel={() => setPendingPage(null)} on_confirm={() => {
      onDirtyChange(false); setPage(pending_page); setPendingPage(null); settings.clearMessages();
    }}><p>当前表单尚未保存，继续后这些修改将丢失。</p></SessionActionDialog>}</>;
});
