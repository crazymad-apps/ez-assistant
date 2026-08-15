import { useState } from "react";
import { Dialog } from "../../../components/Dialog";
import { SelectionPopover } from "../../../components/SelectionPopover";
import type {
  ModelConfiguration,
  ModelKey,
  SessionSummary,
} from "../../../generated/assistant-protocol";
import styles from "./index.module.scss";

type DeleteModelDialogProps = Readonly<{
  blockers: readonly SessionSummary[];
  model: ModelConfiguration;
  onCancel: () => void;
  onConfirm: () => void;
  onReplacementChange: (value: ModelKey | "") => void;
  pending: boolean;
  replacement: ModelKey | "";
  replacements: readonly ModelConfiguration[];
}>;

export function DeleteModelDialog(props: DeleteModelDialogProps) {
  const [replacement_open, setReplacementOpen] = useState(false);
  const replacement_options = props.replacements.flatMap((model) => model.model_key
    ? [{ value: model.model_key, label: model.display_name }]
    : []);
  return (
    <Dialog
      aria_label="删除模型"
      backdrop_class_name={styles.confirm_backdrop}
      dialog_class_name={styles.confirm_dialog}
      dismissible={!props.pending}
      on_close={props.onCancel}
    >
      <header><h4>删除“{props.model.display_name}”？</h4></header>
      {props.blockers.length ? (
        <div className={styles.blocker_list}>
          <p>该模型仍被以下活动会话引用，暂时不能删除：</p>
          <ul>{props.blockers.map((session) => <li key={session.session_id}>{session.title}</li>)}</ul>
        </div>
      ) : (
        <p>删除只影响后续选择，不会改写历史 Run 中已保存的模型信息。</p>
      )}
      {props.model.is_default && !props.blockers.length && (
        <div className={styles.replacement_field}>
          替代默认模型
          <SelectionPopover
            aria_label="选择替代默认模型"
            on_open_change={setReplacementOpen}
            on_select={props.onReplacementChange}
            open={replacement_open}
            options={replacement_options}
            selected={props.replacement}
            trigger_content={replacement_options.find((option) => option.value === props.replacement)?.label ?? "请选择"}
            trigger_variant="field"
          />
        </div>
      )}
      <footer>
        <button onClick={props.onCancel} type="button">{props.blockers.length ? "关闭" : "取消"}</button>
        {!props.blockers.length && (
          <button
            className={styles.danger_primary_button}
            disabled={props.pending || (props.model.is_default && !props.replacement)}
            onClick={props.onConfirm}
            type="button"
          >
            删除模型
          </button>
        )}
      </footer>
    </Dialog>
  );
}

type ConflictDialogProps = Readonly<{
  onCopy: () => void;
  onReload: () => void;
}>;

export function ConflictDialog(props: ConflictDialogProps) {
  return (
    <Dialog
      aria_label="配置已变化"
      backdrop_class_name={styles.confirm_backdrop}
      dialog_class_name={styles.confirm_dialog}
      on_close={props.onReload}
    >
      <header><h4>Runtime 配置已变化</h4></header>
      <p>当前表单基于旧修订，系统没有覆盖磁盘上的新配置。你可以先复制本次输入，再重新加载最新配置。</p>
      <footer>
        <button onClick={props.onCopy} type="button">复制本次输入</button>
        <button className={styles.primary_button} onClick={props.onReload} type="button">重新加载最新配置</button>
      </footer>
    </Dialog>
  );
}
