import { useEffect, useId, useLayoutEffect, useState } from "react";
import type { BackgroundRunSummary, SessionId } from "@ez-assistant/protocol";
import { Dialog } from "../../../components/Dialog";
import { InlineIconButton } from "../../../components/InlineIconButton";
import type { RuntimeClient } from "../../../runtime-client/RuntimeClient";
import { useBackgroundOutput } from "./useBackgroundOutput";
import styles from "./index.module.scss";

export function BackgroundOutputDialog(props: Readonly<{
  client: RuntimeClient;
  session_id: SessionId;
  run: BackgroundRunSummary;
  schedule_execution_id?: string;
  is_stale: boolean;
  on_close: () => void;
  on_end: () => void;
}>) {
  const { client, session_id, schedule_execution_id, run, is_stale, on_close, on_end } = props;
  const title_id = useId();
  const { view, viewport, onScroll, onScrollUp, retry, onReflow } = useBackgroundOutput(client, session_id, run.background_id, on_end, schedule_execution_id);
  const [command, setCommand] = useState<string | null>(null);
  const [command_error, setCommandError] = useState(false);
  const [details_attempt, setDetailsAttempt] = useState(0);

  useEffect(() => {
    // 事件流补齐后重新验证目标。即使弹窗正暂停追读，也能识别错过的结束事件。
    if (is_stale) return;
    const lifetime = new AbortController();
    setCommandError(false);
    const detail_request = schedule_execution_id
      ? client.command({ type: "get_schedule_background", payload: {
        execution_id: schedule_execution_id, background_id: run.background_id,
      } }, { signal: lifetime.signal }).then((result) => result.payload)
      : client.listBackgroundRuns({
        session_id, background_id: run.background_id, include_command: true, limit: 1,
      }, lifetime.signal).then((result) => result.runs[0]);
    void detail_request.then((detail) => {
      if (lifetime.signal.aborted) return;
      if (!detail) on_end();
      else if (detail.command != null) setCommand(detail.command);
      else setCommandError(true);
    }).catch(() => {
      if (!lifetime.signal.aborted) setCommandError(true);
    });
    return () => lifetime.abort();
  }, [client, session_id, schedule_execution_id, run.background_id, is_stale, on_end, details_attempt]);

  const display_command = command ?? run.command_summary;
  // 完整命令太长时在同一滚动视口提供全文，避免元数据挤掉有效的日志阅读区域。
  const command_in_viewport = display_command.length > 360 || display_command.split("\n", 5).length > 4;
  useLayoutEffect(onReflow, [command, onReflow]);
  return (
    <Dialog aria_labelledby={title_id} backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog} on_close={on_close}>
      <header className={styles.dialog_header}>
        <h2 id={title_id}>后台输出</h2>
        <InlineIconButton icon="x" label="关闭后台输出" onClick={on_close} />
      </header>
      <dl className={styles.metadata}>
        <div><dt>命令</dt><dd>
          {command_in_viewport ? <span title={run.command_summary}>全文见输出区顶部</span> : <code>{display_command}</code>}
          {command_error && <button className={styles.text_button} onClick={() => setDetailsAttempt((value) => value + 1)} type="button">重试读取完整命令</button>}
        </dd></div>
        <div><dt>工作目录</dt><dd title={run.working_directory}>{run.working_directory}</dd></div>
        <div><dt>来源 Run</dt><dd title={run.run_id}>{run.run_id}</dd></div>
      </dl>
      <div className={styles.output}>
        <div aria-label="后台日志" className={styles.viewport} onScroll={onScroll}
          onWheel={(event) => { if (event.deltaY < 0) onScrollUp(); }}
          onKeyDown={(event) => { if (["PageUp", "Home", "ArrowUp"].includes(event.key)) onScrollUp(); }}
          ref={viewport} role="region" tabIndex={0}>
          {command_in_viewport && <pre aria-label="完整命令" className={styles.full_command}>{display_command}</pre>}
          <pre className={styles.log}>{view.chunks.map((chunk) => (
            <span data-output-cursor={chunk.cursor} key={chunk.cursor}>{chunk.content}</span>
          ))}</pre>
          {view.ready && !view.chunks.some((chunk) => chunk.content) && <p className={styles.empty_output}>等待输出…</p>}
        </div>
        {view.dropped > 0 && <p className={styles.output_status} role="status">较早输出已滚动清理，本次跳过 {view.dropped} 个字符。</p>}
        {view.loading && <p className={styles.output_status} role="status">正在读取输出…</p>}
        {view.error && <div className={styles.output_status} role="alert">
          <span>{view.error}</span><button className={styles.text_button} onClick={retry} type="button">重试读取</button>
        </div>}
      </div>
    </Dialog>
  );
}
