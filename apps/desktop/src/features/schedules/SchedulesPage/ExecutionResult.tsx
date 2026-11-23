import { useState } from "react";
import type { BackgroundRunSummary, ScheduleDelivery, ScheduleExecution, ScheduleResultFile } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { Icon } from "../../../components/Icon";
import { MarkdownContent } from "../../../components/MarkdownContent";
import { useAppearance } from "../../../stores/AppearanceContext";
import { BackgroundOutputDialog } from "../../context-panel/BackgroundRunsSection/BackgroundOutputDialog";
import type { RuntimeClient } from "../../../runtime-client/RuntimeClient";
import { executionReason } from "./display";
import styles from "./index.module.scss";

export function ExecutionResult(props: Readonly<{
  client: RuntimeClient;
  execution: ScheduleExecution;
  busy: boolean;
  stale: boolean;
  on_stop: () => void;
  on_stop_background: (background_id: string) => void;
  on_retry: () => void;
  on_refresh: () => void;
  on_download: (file: ScheduleResultFile) => void;
  on_bundle_download: () => void;
  on_delivery: (delivery: ScheduleDelivery) => void;
}>) {
  const { execution } = props;
  const theme = useAppearance().preset;
  const [output, setOutput] = useState<BackgroundRunSummary | null>(null);
  const { agent_result, script_result } = execution;
  const reason = script_result?.reason ?? agent_result?.reason;
  const finishing = execution.status === "running" && execution.agent_run_status === "completed"
    && execution.background_runs.length > 0;
  return <section className={styles.result} aria-label="执行结果">
    {execution.status === "running" && <div className={styles.execution_actions}>
      {finishing && <strong>Agent 已完成 · 后台仍运行</strong>}
      <Button disabled={props.busy || props.stale} size="small" onClick={props.on_stop}>停止执行</Button>
    </div>}
    {reason && <p>{executionReason[reason]}</p>}
    {execution.background_runs.length > 0 && <ul className={styles.background}>
      {execution.background_runs.map((run) => <li key={run.background_id}>
        <div><code>{run.command_summary}</code><span>{run.status === "running" ? "运行中" : "进程停止待确认"}</span></div>
        <Button size="small" disabled={!run.log_available || props.stale} onClick={() => setOutput(run)}>查看输出</Button>
        <Button size="small" disabled={props.busy || props.stale} onClick={() => props.on_stop_background(run.background_id)}>停止后台进程</Button>
      </li>)}
    </ul>}
    {output && execution.agent_session_id && execution.background_runs.some((run) => run.background_id === output.background_id) &&
      <BackgroundOutputDialog client={props.client} session_id={execution.agent_session_id}
        schedule_execution_id={execution.execution_id} run={output} is_stale={props.stale}
        on_close={() => setOutput(null)} on_end={() => { setOutput(null); props.on_refresh(); }} />}
    {script_result && execution.status !== "skipped" && <>
      <p>{script_result.exit_code === null ? "未取得退出码" : `退出码 ${script_result.exit_code}`}
        {script_result.truncated && " · 输出已截断"}</p>
      <Log label="标准输出" content={script_result.stdout} />
      <Log label="标准错误" content={script_result.stderr} />
    </>}
    {agent_result?.report && <>
      <h4>任务汇报</h4>
      <MarkdownContent text={agent_result.report} theme={theme} />
    </>}
    {execution.files.length > 0 && <div className={styles.files}>
      <h4>交付文件</h4>
      <ul>{execution.files.map((file) => <li key={file.file_id}>
        <Button size="small" variant="text" disabled={props.busy || props.stale} onClick={() => props.on_download(file)}>
          <Icon name="file" size={16} /> {file.title || file.name}
        </Button>
      </li>)}
        {execution.files.length > 1 && <li><Button size="small" variant="text" disabled={props.busy || props.stale}
          onClick={props.on_bundle_download}>下载全部文件</Button></li>}
      </ul>
    </div>}
    {execution.status !== "running" && execution.status !== "skipped" &&
      <div className={styles.delivery}>
        {execution.delivery ? <Button size="small" disabled={props.busy || props.stale}
          onClick={() => props.on_delivery(execution.delivery!)}>前往助理会话 <Icon name="external-link" size={14} /></Button>
          : execution.settings.report_to_controller ? <span>等待投递至助理会话</span>
            : <span>仅保存在执行记录</span>}
        {!execution.result_finalized && (execution.result_error
          ? <div role="alert"><p>{execution.result_error}</p>
            <Button size="small" disabled={props.busy || props.stale} onClick={props.on_retry}>重试结果处理</Button>
          </div>
          : <span>{execution.delivery ? "正在清理临时资源…" : "正在处理结果…"}</span>)}
      </div>}
  </section>;
}

function Log({ label, content }: Readonly<{ label: string; content: string }>) {
  return <div className={styles.logs}>
    <h4>{label}</h4>
    {content ? <pre aria-label={label} tabIndex={0}>{content}</pre> : <p className={styles.hint}>无输出</p>}
  </div>;
}
