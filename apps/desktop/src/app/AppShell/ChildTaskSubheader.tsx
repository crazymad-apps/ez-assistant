import { observer } from "mobx-react-lite";
import { Icon } from "../../components/Icon";
import {
  childTaskStatusLabel,
  formatCompactTokens,
  mergeChildTaskItems,
} from "../../features/conversation/childTaskPresentation";
import { useRootStore } from "../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const ChildTaskSubheader = observer(function ChildTaskSubheader() {
  const store = useRootStore();
  const session_id = store.navigation.selected_session_id;
  const child_task_id = store.navigation.selected_child_task_id;
  if (!session_id || !child_task_id) {
    return null;
  }

  const session_view = store.projection.session_views.get(session_id);
  const child_view = store.projection.child_task_views.get(child_task_id);
  const reliable_items = child_view
    ? [...(session_view?.child_tasks ?? []), child_view.task]
    : session_view?.child_tasks ?? [];
  const item = mergeChildTaskItems(
    reliable_items,
    store.live_execution.childTasksForSession(session_id),
    (task_id) => store.live_execution.runForChildTask(task_id),
  ).find((candidate) => candidate.task.child_task_id === child_task_id);

  if (!item) {
    return null;
  }

  const total_tokens = item.usage.accumulated?.total_tokens;
  return (
    <section aria-label="子任务标题栏" className={styles.child_task_header}>
      <button
        aria-label="返回主会话"
        className={styles.child_back}
        onClick={() => store.closeChildTask()}
        type="button"
      >
        <Icon name="chevron-left" size={15} />
      </button>
      <div className={styles.child_title}>
        <strong title={item.task.title}>{item.task.title}</strong>
        <span className={styles.child_status} data-status={item.task.status}>
          <i aria-hidden="true" />
          {childTaskStatusLabel(item.task.status)}
        </span>
      </div>
      {total_tokens !== null && total_tokens !== undefined && (
        <span className={styles.child_usage}>{formatCompactTokens(total_tokens)}</span>
      )}
      {item.can_cancel && (
        <button
          className={styles.child_cancel}
          disabled={item.task.cancel_requested}
          onClick={() => void store.cancelChildTask(session_id, child_task_id)}
          type="button"
        >
          {item.task.cancel_requested ? "停止中" : "停止"}
        </button>
      )}
    </section>
  );
});
