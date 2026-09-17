import { observer } from "mobx-react-lite";
import type {
  ApprovalDecision,
  ApprovalSnapshot,
  ToolApprovalSubject,
  ToolInputProjection,
  ToolInputSnapshot,
} from "@ez-assistant/protocol";
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
          <small>{props.approval.child_task_id ? `${props.child_title ?? "子任务"} · 由子智能体请求` : approvalContext(subject)}</small>
        </div>
        {props.remaining > 0 && <b>另有 {props.remaining} 项</b>}
        <button aria-label="最小化授权面板" onClick={props.on_minimize} type="button">—</button>
      </header>
      <div className={styles.approval_body}>
        <section>
          <h4>请求内容</h4>
          <ApprovalInput input={props.approval.input} />
          {subject.type === "mcp" && subject.untrusted_annotations_json && (
            <details><summary>服务自报的工具注解（未经验证）</summary><pre>{subject.untrusted_annotations_json}</pre></details>
          )}
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

function ApprovalInput({ input }: Readonly<{ input: ToolInputProjection }>) {
  return <div>
    <ApprovalInputValue input={input.value} />
    {input.redacted && <p>已隐去敏感值。</p>}
    {input.truncated && <p>内容已截断。</p>}
  </div>;
}

function ApprovalInputValue({ input }: Readonly<{ input: ToolInputSnapshot }>) {
  switch (input.type) {
    case "shell":
      return <><code className={styles.command_line}><Icon name="terminal" size={15} />{input.command}</code><p>{input.working_directory}</p></>;
    case "file":
      return <p>{input.operation} · <code>{input.path}</code></p>;
    case "files":
      return <div><p>{input.operation} · {input.paths.length} 个文件</p>{input.paths.map((path) => <p key={path}><code>{path}</code></p>)}</div>;
    case "delegation":
      return <p>{input.title} · {input.task_summary}</p>;
    case "mcp":
      return <div className={styles.mcp_approval}>
        <p>{input.identity.server_display_name} ({input.identity.server_key}) / <code>{input.identity.tool_name}</code></p>
        <pre>{input.arguments_json}</pre>
        <p>工具注解由 MCP 服务提供，不能作为安全或只读保证。外部工具可能产生副作用，请核对参数后授权。</p>
      </div>;
    case "image_inspection":
      return <p>{input.goal} · {input.image_paths.join("、")}</p>;
    case "general":
      return <pre>{input.summary}</pre>;
    case "unavailable":
      return <p>参数不可用，无法安全展示。</p>;
  }
}

function approvalQuestion(subject: ToolApprovalSubject): string {
  if (subject.type === "mcp") return `允许调用 ${subject.identity.server_key} / ${subject.identity.tool_name}？`;
  return subject.type === "shell" ? "允许执行命令行指令？" : `允许执行 ${subject.tool_name}？`;
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
  return decision === "allow_once" ? "仅本次" : decision === "allow_session" ? "当前会话" : "当前工作区";
}

function approvalDecisionDescription(decision: ApprovalDecision): string {
  return decision === "allow_once"
    ? "只允许当前这次请求"
    : decision === "allow_session"
      ? "本会话内相同操作不再询问"
      : "当前工作区内相同操作不再询问";
}
