import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useRootStore } from '../../../stores/RootStoreContext';
import { ModelPicker, modelSelectionIssue, modelSelectionLabel } from '../ModelPicker';
import styles from './index.module.scss';

export const ModelSelectionControl = observer(function ModelSelectionControl(props: Readonly<{
  purpose: 'default' | 'vision';
  read_only?: boolean;
}>) {
  const settings = useRootStore().settings;
  const [open, setOpen] = useState(false);
  const vision = props.purpose === 'vision';
  const selection = vision ? settings.model_settings.vision_model : settings.model_settings.default_model;
  const title = vision ? '默认识图模型' : props.read_only ? '组织默认模型' : '默认模型';
  const enterprise = Boolean(settings.model_settings.management);
  const issue = modelSelectionIssue(selection, settings.providers, settings.model_settings)
    ?? (props.read_only ? settings.model_settings.management?.unavailable_reason : null);
  const label = selection ? modelSelectionLabel(selection, settings.providers, enterprise && vision) : '未配置';
  const display = selection && issue ? `${label}（不可用）` : label;
  return (
    <div className={styles.model_selection}>
      <div className={styles.model_selection_row} role={props.read_only ? 'group' : undefined} aria-label={props.read_only ? title : undefined}>
        <strong>{title}</strong>
        {props.read_only ? <span className={styles.model_selection_value}>{display}</span> : (
          <div className={styles.model_selection_value}>
            <ModelPicker
              providers={settings.providers}
              selection={selection}
              title={title}
              label={display}
              open={open}
              onOpenChange={setOpen}
              vision_only={vision}
              clearable
              show_manage_providers={false}
              disabled={settings.pending_action !== null}
              onSelect={(next) => vision ? settings.setAuxiliaryVisionModel(next) : settings.setDefaultModel(next)}
              trigger_class_name={styles.vision_model_select}
            />
          </div>
        )}
      </div>
      {issue && <p className={styles.model_selection_issue} role="status">{issue}</p>}
    </div>
  );
});
