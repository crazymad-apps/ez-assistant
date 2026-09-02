import { useState } from "react";
import type { GoalPauseReasonSnapshot, GoalSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { PresenceBoundary } from "../../../components/Presence";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { ComposerSecondaryDrawer } from "./ComposerSecondaryDrawer";
import { formatCompact } from "./composerOptions";
import styles from "./index.module.scss";

type GoalStatusRowProps = Readonly<{
  goal: GoalSnapshot;
  on_clear: () => Promise<boolean>;
  on_open_change: (open: boolean) => void;
  on_resume: () => Promise<boolean>;
  on_stop: () => Promise<boolean>;
  open: boolean;
  pending: boolean;
}>;

export function GoalStatusRow(props: GoalStatusRowProps) {
  const action = goalAction(props.goal);
  const [clear_open, setClearOpen] = useState(false);

  async function runAction() {
    if (!action) return;
    let succeeded: boolean;
    if (action === "stop") {
      succeeded = await props.on_stop();
    } else {
      succeeded = await props.on_resume();
    }
    if (succeeded) props.on_open_change(false);
  }

  async function clearGoal() {
    if (await props.on_clear()) {
      setClearOpen(false);
      props.on_open_change(false);
    }
  }

  return (
    <>
    <ComposerSecondaryDrawer
      actions={<>
        {action && (
          <button
            className={styles.goal_primary_action}
            data-action={action}
            disabled={props.pending}
            onClick={() => void runAction()}
            type="button"
          >
            {goalActionLabel(action)}
          </button>
        )}
        {props.goal.state !== "running" && (
          <button
            aria-label="退出目标"
            className={styles.goal_close}
            disabled={props.pending}
            onClick={() => setClearOpen(true)}
            title="退出目标"
            type="button"
          >
            <Icon name="x" size={14} />
          </button>
        )}
      </>}
      label="目标"
      on_open_change={props.on_open_change}
      open={props.open}
      state={props.goal.state}
      summary={<>
        <i className={styles.goal_indicator} />
        <strong className={styles.goal_state}>{goalStateLabel(props.goal)}</strong>
        <span className={styles.goal_objective} title={props.goal.objective_preview}>{props.goal.objective_preview}</span>
        <small className={styles.secondary_drawer_meta}>{remainingRuns(props.goal)} 次运行</small>
      </>}
    >
      <section aria-label="目标详情" className={styles.goal_detail} role="region">
        <div className={styles.goal_detail_body}>
          <dl>
            <div><dt>当前轮次</dt><dd>{props.goal.turn}</dd></div>
            <div><dt>运行次数预算</dt><dd>{props.goal.budget.used_runs} / {props.goal.budget.max_runs}</dd></div>
            <div><dt>令牌预算</dt><dd>{formatCompact(props.goal.budget.used_total_tokens)} / {formatCompact(props.goal.budget.max_total_tokens)}</dd></div>
            <div><dt>连续失败</dt><dd>{props.goal.budget.consecutive_failures} / {props.goal.budget.max_consecutive_failures}</dd></div>
            <div><dt>附件</dt><dd>{props.goal.attachment_count}</dd></div>
          </dl>
          {props.goal.pause_reason && (
            <p className={styles.goal_pause_reason}>{pauseReasonLabel(props.goal.pause_reason)}</p>
          )}
          {!props.goal.budget.usage_complete && <p className={styles.goal_usage_note}>部分服务商未报告完整令牌用量，客户端不会猜测。</p>}
        </div>
      </section>
    </ComposerSecondaryDrawer>
    <PresenceBoundary present={clear_open}>
      {clear_open && (
        <SessionActionDialog
          confirm_label="退出目标"
          is_danger
          is_pending={props.pending}
          on_cancel={() => setClearOpen(false)}
          on_confirm={() => void clearGoal()}
          title="退出当前目标？"
        >
          <p>自动续跑控制状态会被清除，任务清单和排队输入会保留。</p>
        </SessionActionDialog>
      )}
    </PresenceBoundary>
    </>
  );
}

type GoalAction = "stop" | "resume";

function goalAction(goal: GoalSnapshot): GoalAction | null {
  if (goal.state === "running") return "stop";
  if (goal.state === "paused") return "resume";
  return null;
}

function goalActionLabel(action: GoalAction): string {
  if (action === "stop") return "停止";
  return "继续";
}

function goalStateLabel(goal: GoalSnapshot): string {
  if (goal.state === "running") return "目标自动推进中";
  if (goal.state === "completed") return "目标已完成";
  if (goal.pause_reason?.type === "blocked") return "目标等待输入";
  return "目标已暂停";
}

function remainingRuns(goal: GoalSnapshot): number {
  return Math.max(0, goal.budget.max_runs - goal.budget.used_runs);
}

function pauseReasonLabel(reason: GoalPauseReasonSnapshot): string {
  switch (reason.type) {
    case "blocked":
      return `等待用户：${reason.summary}`;
    case "user_stopped":
      return "已由用户停止，可显式继续。";
    case "run_limit_reached":
      return "已达到运行次数预算上限。";
    case "token_limit_reached":
      return "已达到令牌预算上限。";
    case "consecutive_failures":
      return "连续执行失败达到保护上限。";
    case "recovery_required":
      return "运行时重启后需要显式恢复。";
    case "forked":
      return "这是分叉后的独立目标，需要显式恢复。";
  }
}
