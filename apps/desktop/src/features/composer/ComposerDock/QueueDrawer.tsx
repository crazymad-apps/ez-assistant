import { observer } from "mobx-react-lite";
import type { GoalSnapshot, QueueSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ComposerSecondaryDrawer } from "./ComposerSecondaryDrawer";
import styles from "./index.module.scss";

export const QueueDrawer = observer(function QueueDrawer(props: Readonly<{
  open: boolean;
  on_open_change: (open: boolean) => void;
  goal: GoalSnapshot | null;
  queue: QueueSnapshot;
  session_id: string;
}>) {
  const store = useRootStore();
  const needs_resume = props.queue.state !== "automatic";
  const held_count = props.queue.items.filter((item) => item.held_by_goal).length;
  return (
    <ComposerSecondaryDrawer
      label="输入队列"
      on_open_change={props.on_open_change}
      open={props.open}
      summary={<>
        <Icon name="fork" size={16} />
        <strong>{held_count > 0 ? "待处理指导" : needs_resume ? "待恢复队列" : "待执行队列"}</strong>
        <b className={styles.secondary_drawer_count}>{props.queue.items.length}</b>
      </>}
    >
      <div className={styles.queue_items}>
          {needs_resume && held_count === 0 && (
            <button
              disabled={store.pending_queue_input_id !== null}
              onClick={() => void store.resumeAllQueuedInputs(props.session_id, props.queue.revision)}
              type="button"
            >
              {props.queue.state === "resume_required" ? "确认并继续全部" : "继续全部"}
            </button>
          )}
          {props.queue.items.map((item) => (
            <div className={styles.queue_item} key={item.input_id}>
              <span>{item.position}</span>
              <p title={item.text_preview}>{item.text_preview}</p>
              <small>{item.held_by_goal ? "目标暂存" : ""}</small>
              <div className={styles.queue_actions}>
                {item.skill && <span className={styles.queue_skill} title={item.skill.name}>{item.skill.name}</span>}
                <time>{formatTime(item.submitted_at_ms)}</time>
                <button
                  disabled={
                    store.pending_queue_input_id !== null
                    || Boolean(item.held_by_goal && props.goal?.state !== "paused")
                  }
                  onClick={() => {
                    if (item.held_by_goal && props.goal?.state === "paused") {
                      void store.resumeGoal(props.session_id, props.goal.goal_id, props.goal.generation, item.input_id);
                    } else if (needs_resume) {
                      void store.resumeQueuedInput(props.session_id, item.input_id, props.queue.revision);
                    } else if (!item.held_by_goal) {
                      void store.prioritizeQueuedInput(props.session_id, item.input_id, props.queue.revision);
                    }
                  }}
                  type="button"
                >
                  {item.held_by_goal
                    ? props.goal?.state === "paused" ? "用于目标" : "已暂存"
                    : needs_resume ? "恢复" : item.is_prioritized ? "已优先" : "优先"}
                </button>
                <button
                  aria-label="移除排队输入"
                  disabled={store.pending_queue_input_id !== null}
                  onClick={() => void store.cancelQueuedInput(props.session_id, item.input_id)}
                  type="button"
                >
                  <Icon name="x" size={14} />
                </button>
              </div>
            </div>
          ))}
      </div>
    </ComposerSecondaryDrawer>
  );
});

function formatTime(value: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(value);
}
