import type { ChildTaskTreeItemSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { childTaskStatusLabel, formatCompactTokens } from "../childTaskPresentation";
import styles from "./index.module.scss";

export function ChildTaskTree({ embedded = false, items, on_open }: Readonly<{
  embedded?: boolean;
  items: readonly ChildTaskTreeItemSnapshot[];
  on_open: (item: ChildTaskTreeItemSnapshot) => void;
}>) {
  if (items.length === 0) {
    return null;
  }
  return (
    <section className={styles.task_tree} data-embedded={embedded} aria-label="子任务">
      {!embedded && (
        <div className={styles.task_tree_root}>
          <Icon name="bot" size={15} />
          <span>当前主会话</span>
          <span className={styles.task_tree_line} aria-hidden="true" />
        </div>
      )}
      {items.map((item) => (
        <div className={styles.child_task} data-embedded={embedded} key={item.task.child_task_id}>
          {!embedded && <span className={styles.task_branch} aria-hidden="true" />}
          <button aria-label={`查看子任务：${item.task.title}`} onClick={() => on_open(item)} type="button">
            <i data-status={item.task.status} />
            <span className={styles.task_text}>
              <strong>{item.task.title}</strong>
              <small>{childTaskStatusLabel(item.task.status)}{item.pending_approval_count > 0 ? ` · ${item.pending_approval_count} 项待审批` : ""}</small>
            </span>
            {item.usage.accumulated?.total_tokens !== null && item.usage.accumulated?.total_tokens !== undefined && (
              <span className={styles.task_usage}>{formatCompactTokens(item.usage.accumulated.total_tokens)}</span>
            )}
            <Icon name="chevron-right" size={14} />
          </button>
        </div>
      ))}
    </section>
  );
}
