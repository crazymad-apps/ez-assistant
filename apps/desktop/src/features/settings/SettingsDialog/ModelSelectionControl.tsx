import { observer } from "mobx-react-lite";
import { useState } from "react";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ModelPicker } from "../ModelPicker";
import styles from "./index.module.scss";

export const ModelSelectionControl = observer(function ModelSelectionControl(props: Readonly<{ purpose: "default" | "vision" }>) {
  const settings = useRootStore().settings;
  const [open, setOpen] = useState(false);
  const selection = props.purpose === "default" ? settings.model_settings.default_model : settings.model_settings.vision_model;
  const title = props.purpose === "default" ? "默认模型" : "默认识图模型";
  const provider = settings.providers.find((item) => item.provider_instance_id === selection?.provider_instance_id);
  const label = selection ? `${provider?.connection.display_name ?? "服务商已删除"} / ${selection.model_id}` : "未配置";
  const select = (next: typeof selection) => props.purpose === "default" ? settings.setDefaultModel(next) : settings.setAuxiliaryVisionModel(next);
  return <div className={styles.diagnostic_card}><strong>{title}</strong><div className={styles.runtime_actions}>
    <ModelPicker providers={settings.providers} selection={selection} title={title} label={label} open={open} onOpenChange={setOpen}
      clearable show_manage_providers={false} disabled={settings.pending_action !== null} onSelect={select} trigger_class_name={styles.vision_model_select} />
  </div></div>;
});
