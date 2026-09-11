import { useState } from "react";
import { Button } from "../../../../components/Button";
import type { ModelSelection, ProviderSummary } from "@ez-assistant/protocol";
import { ModelFixedConfigPage } from "../ModelFixedConfigPage";
import { SettingsPageContainer } from "../SettingsPageContainer";
import shared from "../index.module.scss";
import styles from "./index.module.scss";

/** ID 确认只进入参数草稿，保存仍由详情页负责；取消沿用外层未保存确认。 */
export function ManualModelPage(props: Readonly<{
  provider: ProviderSummary;
  onBack: () => void; onDeleted: () => void; onDirtyChange: (dirty: boolean) => void;
}>) {
  const [model_id, setModelId] = useState("");
  const [selection, setSelection] = useState<ModelSelection | null>(null);
  const normalized_id = model_id.trim();
  if (selection) return <ModelFixedConfigPage {...props} selection={selection} manual />;
  return <SettingsPageContainer title="添加模型" on_back={props.onBack}>
    <form className={styles.form} onSubmit={(event) => {
      event.preventDefault();
      if (normalized_id) setSelection({ provider_instance_id: props.provider.provider_instance_id, model_id: normalized_id });
    }}>
      <div className={shared.model_form}>
        <label>模型 ID
          <input autoFocus aria-label="模型 ID" required maxLength={1024} autoComplete="off" spellCheck={false}
            placeholder="请输入服务商支持的模型 ID" value={model_id}
            onChange={(event) => { setModelId(event.target.value); props.onDirtyChange(Boolean(event.target.value.trim())); }} />
        </label>
      </div>
      <footer className={styles.actions}>
        <Button type="button" onClick={props.onBack}>取消</Button>
        <Button type="submit" variant="primary" disabled={!normalized_id}>配置参数</Button>
      </footer>
    </form>
  </SettingsPageContainer>;
}
