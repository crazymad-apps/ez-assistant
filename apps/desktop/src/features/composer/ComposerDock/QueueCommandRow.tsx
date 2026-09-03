import type { QueuedSessionCommandSnapshot } from "../../../generated/assistant-protocol";
import styles from "./index.module.scss";

/** Command 只显示队列状态，不暴露取消或伪 Run 操作。 */
export function QueueCommandRow(props: Readonly<{
  item: QueuedSessionCommandSnapshot;
  disabled: boolean;
  held_by_goal: boolean;
  needs_resume: boolean;
  on_prioritize: () => void;
  on_resume: () => void;
}>) {
  const executing = props.item.state === "executing";
  const label = `MCP 刷新：${props.item.command.payload.server ?? "全部"}`;
  let action_label = props.item.is_prioritized ? "已优先" : "优先";
  let on_action = props.on_prioritize;
  if (props.needs_resume && !props.held_by_goal) {
    action_label = "恢复";
    on_action = props.on_resume;
  }
  return (
    <div className={styles.queue_item}>
      <span>{props.item.position}</span>
      <p title={label}>{label}</p>
      <small>{props.held_by_goal ? "等待目标结束" : "控制指令"}</small>
      <div className={styles.queue_actions}>
        {executing
          ? <span aria-label="正在刷新 MCP" className={styles.loading_ring} role="status" />
          : <button disabled={props.disabled || (props.item.is_prioritized && !props.needs_resume)} onClick={on_action} type="button">{action_label}</button>}
      </div>
    </div>
  );
}
