import { observer } from "mobx-react-lite";
import type {
  ApprovalDecision,
  ApprovalSnapshot,
  ToolApprovalSubject,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const ApprovalWorkspace = observer(function ApprovalWorkspace(props: Readonly<{
  approval: ApprovalSnapshot;
  child_title: string | null;
  decision: ApprovalDecision | null;
  on_decision_change: (decision: ApprovalDecision) => void;
  on_minimize: () => void;
  queue_revision: number;
  remaining: number;
}>) {
  const store = useRootStore();
  const allow_options = props.approval.available_decisions.filter(isAllowDecision);
  const pending = store.pending_approval_id === props.approval.approval_id;
  const subject = props.approval.subject;
  return (
    <section className={styles.approval_workspace}>
      <header>
        <span><Icon name="shield" size={17} /></span>
        <div>
          <strong>{approvalQuestion(subject)}</strong>
          <small>{props.approval.child_task_id ? `${props.child_title ?? "子任务"} · 由子 Agent 请求` : approvalContext(subject)}</small>
        </div>
        {props.remaining > 0 && <b>另有 {props.remaining} 项</b>}
        <button aria-label="最小化授权面板" onClick={props.on_minimize} type="button">—</button>
      </header>
      <div className={styles.approval_body}>
        <section>
          <h4>请求内容</h4>
          <ApprovalSubject subject={subject} />
        </section>
        {allow_options.length > 0 && (
          <fieldset>
            <legend>授权范围</legend>
            {allow_options.map((decision) => (
              <label data-selected={props.decision === decision} key={decision}>
                <input checked={props.decision === decision} name="approval-scope" onChange={() => props.on_decision_change(decision)} type="radio" />
                <span><b>{approvalDecisionLabel(decision)}</b><small>{approvalDecisionDescription(decision)}</small></span>
              </label>
            ))}
          </fieldset>
        )}
      </div>
      <footer>
        <button className={styles.stop_run_button} disabled={pending} onClick={() => void store.rejectApprovalAndStopRun(props.approval.session_id, props.approval.approval_id, props.queue_revision)} type="button">拒绝并停止本轮</button>
        <span />
        <button disabled={pending} onClick={() => void store.decideApproval(props.approval.session_id, props.approval.approval_id, "deny")} type="button">拒绝</button>
        <button className={styles.allow_button} disabled={pending || !props.decision} onClick={() => props.decision && void store.decideApproval(props.approval.session_id, props.approval.approval_id, props.decision)} type="button">允许执行</button>
      </footer>
    </section>
  );
});

export function isAllowDecision(decision: ApprovalDecision): boolean {
  return decision !== "deny";
}

function ApprovalSubject({ subject }: Readonly<{ subject: ToolApprovalSubject }>) {
  if (subject.type === "shell") {
    return <><code className={styles.command_line}><Icon name="terminal" size={15} />{subject.command}</code><p>{subject.working_directory}</p></>;
  }
  if (subject.type === "file") {
    return <p>{subject.operation} · <code>{subject.path}</code></p>;
  }
  if (subject.type === "delegation") {
    return <p>{subject.title} · {subject.task_summary}</p>;
  }
  return <p>{subject.tool_name}</p>;
}

function approvalQuestion(subject: ToolApprovalSubject): string {
  return subject.type === "shell" ? "允许执行 Shell 命令？" : `允许执行 ${subject.tool_name}？`;
}

function approvalContext(subject: ToolApprovalSubject): string {
  if (subject.type === "shell") return subject.working_directory;
  if (subject.type === "general") {
    if (subject.tool_name === "list_pinned_memories") return "读取置顶记忆的最新状态";
    if (["pin_memory", "update_pinned_memory", "unpin_memory"].includes(subject.tool_name)) {
      return "修改将影响未来新建会话使用的置顶记忆";
    }
  }
  return "当前会话";
}

function approvalDecisionLabel(decision: ApprovalDecision): string {
  return decision === "allow_once" ? "仅本次" : decision === "allow_session" ? "当前会话" : "当前 Workspace";
}

function approvalDecisionDescription(decision: ApprovalDecision): string {
  return decision === "allow_once"
    ? "只允许当前这次请求"
    : decision === "allow_session"
      ? "本会话内相同操作不再询问"
      : "当前工作区内相同操作不再询问";
}
