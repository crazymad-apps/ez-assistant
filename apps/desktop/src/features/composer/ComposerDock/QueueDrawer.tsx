import { observer } from "mobx-react-lite";
import { shellLabels } from "../../../runtime-client/shellPresentation";
import type { GoalSnapshot, QueueSnapshot } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
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
  intervention_available: boolean;
  intervention_blocked: boolean;
  queue: QueueSnapshot;
  presentation: QueuePresentation;
  session_id: string;
}>) {
  const store = useRootStore();
  const queue_pending = store.pending_queue_input_id !== null || store.intervention?.target.type === 'queue';
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
          {needs_resume && held_count === 0 && props.goal?.state !== "running" && (
            <button
              disabled={queue_pending || executing_command}
              onClick={() => void store.resumeAllQueuedInputs(props.session_id, props.queue.revision)}
              type="button"
            >
              {props.queue.state === "resume_required" ? "确认并继续全部" : "继续全部"}
            </button>
          )}
          {props.presentation.items.map((entry) => {
            if (entry.type === "command") {
              return <QueueCommandRow
                disabled={queue_pending || executing_command}
                held_by_goal={props.goal?.state === "running"}
                item={entry.payload}
                key={entry.payload.input_id}
                needs_resume={needs_resume}
                on_prioritize={() => void store.prioritizeQueuedInput(props.session_id, entry.payload.input_id, props.queue.revision)}
                on_resume={() => void store.resumeQueuedInput(props.session_id, entry.payload.input_id, props.queue.revision)}
              />;
            }
            const item = entry.payload;
            const user_message = !item.agent_shell_target && item.source.type !== 'controller_delivery' && item.source.type !== 'proxy_report';
            return (
            <div className={styles.queue_item} key={item.input_id}>
              <span>{item.position}</span>
              <p title={item.text_preview}>{item.agent_shell_target ? `切换 Agent Shell → ${shellLabels[item.agent_shell_target]}` : item.text_preview}</p>
              <small>{queueSourceLabel(item.source, store.projection) ?? (item.held_by_goal ? "目标暂存" : "")}</small>
              <div className={styles.queue_actions}>
                {item.skill && <span className={styles.queue_skill} title={item.skill.name}>{item.skill.name}</span>}
                {item.mcp_selection && <span className={styles.queue_skill} title={item.mcp_selection.server_key}>MCP · {item.mcp_selection.display_name}</span>}
                <time>{formatTime(item.submitted_at_ms)}</time>
                <Button
                  disabled={
                    queue_pending
                    || executing_command
                    || item.held_by_goal
                  }
                  onClick={() => {
                    if (needs_resume) {
                      void store.resumeQueuedInput(props.session_id, item.input_id, props.queue.revision);
                    } else {
                      void store.prioritizeQueuedInput(props.session_id, item.input_id, props.queue.revision);
                    }
                  }}
                  size="small"
                  variant="text"
                >
                  {item.held_by_goal
                    ? "已暂存"
                    : needs_resume ? "恢复" : item.is_prioritized ? "已优先" : "优先"}
                </Button>
                {props.intervention_available && user_message && (
                  <Button
                    aria-label="介入"
                    disabled={queue_pending || props.intervention_blocked || Boolean(store.intervention) || store.composer_pending
                      || executing_command || store.connection.state !== 'connected' || store.projection.is_stale}
                    iconOnly
                    onClick={() => void store.interveneQueuedInput(props.session_id, item.input_id)}
                    size="small"
                    title="介入"
                    variant="text"
                  >
                    <Icon name="corner-down-right" size={16} />
                  </Button>
                )}
                <Button
                  aria-label="移除排队输入"
                  disabled={queue_pending}
                  iconOnly
                  onClick={() => void store.cancelQueuedInput(props.session_id, item.input_id)}
                  size="small"
                  variant="text"
                >
                  <Icon name="x" size={16} />
                </Button>
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
  source: import("@ez-assistant/protocol").ConversationInputSourceSnapshot,
  projection: import("../../../stores/RuntimeProjectionStore").RuntimeProjectionStore,
): string | null {
  if (source.type === "controller_delivery") {
    return "主控转达";
  }
  if (source.type === "proxy_report") {
    const session = projection.sessionSummary(source.source_session_id);
    return `会话报告 · ${session?.title ?? "来源会话"}`;
  }
  return null;
}
