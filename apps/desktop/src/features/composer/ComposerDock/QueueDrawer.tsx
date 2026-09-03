import { observer } from "mobx-react-lite";
import type { GoalSnapshot, QueueSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ComposerSecondaryDrawer } from "./ComposerSecondaryDrawer";
import { QueueCommandRow } from "./QueueCommandRow";
import styles from "./index.module.scss";
import type { QueuePresentation } from "./queuePresentation";

export const QueueDrawer = observer(function QueueDrawer(props: Readonly<{
  open: boolean;
  on_open_change: (open: boolean) => void;
  goal: GoalSnapshot | null;
  queue: QueueSnapshot;
  presentation: QueuePresentation;
  session_id: string;
}>) {
  const store = useRootStore();
  const needs_resume = props.queue.state !== "automatic";
  const held_count = props.presentation.items.filter((item) => item.type === "message" && item.payload.held_by_goal).length;
  const executing_command = props.queue.items.some((item) => item.type === "command" && item.payload.state === "executing");
  return (
    <ComposerSecondaryDrawer
      label="输入队列"
      on_open_change={props.on_open_change}
      open={props.open}
      summary={<>
        <Icon name="fork" size={16} />
        <strong>{held_count > 0 ? "待处理指导" : needs_resume ? "待恢复队列" : "待执行队列"}</strong>
        <b className={styles.secondary_drawer_count}>{props.presentation.count}</b>
      </>}
    >
      <div className={styles.queue_items}>
          {needs_resume && held_count === 0 && !props.goal && (
            <button
              disabled={store.pending_queue_input_id !== null || executing_command}
              onClick={() => void store.resumeAllQueuedInputs(props.session_id, props.queue.revision)}
              type="button"
            >
              {props.queue.state === "resume_required" ? "确认并继续全部" : "继续全部"}
            </button>
          )}
          {props.presentation.items.map((entry) => {
            if (entry.type === "command") {
              return <QueueCommandRow
                disabled={store.pending_queue_input_id !== null || executing_command}
                held_by_goal={Boolean(props.goal)}
                item={entry.payload}
                key={entry.payload.input_id}
                needs_resume={needs_resume}
                on_prioritize={() => void store.prioritizeQueuedInput(props.session_id, entry.payload.input_id, props.queue.revision)}
                on_resume={() => void store.resumeQueuedInput(props.session_id, entry.payload.input_id, props.queue.revision)}
              />;
            }
            const item = entry.payload;
            return (
            <div className={styles.queue_item} key={item.input_id}>
              <span>{item.position}</span>
              <p title={item.text_preview}>{item.text_preview}</p>
              <small>{queueSourceLabel(item.source, store.projection.application) ?? (item.held_by_goal ? "目标暂存" : "")}</small>
              <div className={styles.queue_actions}>
                {item.skill && <span className={styles.queue_skill} title={item.skill.name}>{item.skill.name}</span>}
                {item.mcp_selection && <span className={styles.queue_skill} title={item.mcp_selection.server_key}>MCP · {item.mcp_selection.display_name}</span>}
                <time>{formatTime(item.submitted_at_ms)}</time>
                <button
                  disabled={
                    store.pending_queue_input_id !== null
                    || executing_command
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
          ); })}
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

function queueSourceLabel(
  source: import("../../../generated/assistant-protocol").ConversationInputSourceSnapshot,
  application: import("../../../generated/assistant-protocol").ApplicationSnapshot | null,
): string | null {
  if (source.type === "controller_delivery") {
    return "主控转达";
  }
  if (source.type === "proxy_report") {
    const session = [...(application?.active_sessions ?? []), ...(application?.archived_sessions ?? [])]
      .find((candidate) => candidate.session_id === source.source_session_id);
    return `会话报告 · ${session?.title ?? "来源会话"}`;
  }
  return null;
}
