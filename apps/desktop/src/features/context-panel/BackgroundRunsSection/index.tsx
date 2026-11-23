import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useRef, useState } from "react";
import { EmptyRow } from "../../../components/EmptyRow";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { ContextSection } from "../ContextPanel/ContextSection";
import { BackgroundOutputDialog } from "./BackgroundOutputDialog";
import { BACKGROUND_PAGE_SIZE, type BackgroundRunsStore } from "./store";
import styles from "./index.module.scss";

export const BackgroundRunsSection = observer(function BackgroundRunsSection(props: Readonly<{
  is_open: boolean;
  is_stale: boolean;
  runs: BackgroundRunsStore;
  visible: boolean;
  on_toggle: () => void;
}>) {
  const { runs, is_open, is_stale, visible, on_toggle } = props;
  const browsing = visible && is_open;
  const has_rows = runs.rows.length > 0;
  const heading = useRef<HTMLButtonElement>(null);
  const [now, setNow] = useState(Date.now);

  useEffect(() => { if (!browsing) runs.close(); }, [browsing, runs]);

  useEffect(() => {
    if (!browsing || !has_rows) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [browsing, has_rows]);

  const notice = runs.notice;
  useEffect(() => {
    if (!notice || !browsing) return;
    // 弹窗的来源行在任务结束时一并移除。此时给键盘焦点一个仍存在的入口。
    const frame = requestAnimationFrame(() => {
      if (document.activeElement === document.body) heading.current?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [browsing, notice]);

  const selected_id = runs.selected?.background_id;
  const close = useCallback(() => runs.close(), [runs]);
  const end = useCallback(() => { if (selected_id) runs.ended(selected_id); }, [runs, selected_id]);
  const pages = Math.max(1, Math.ceil((runs.total ?? 0) / BACKGROUND_PAGE_SIZE));
  const title = runs.total === null ? "后台运行" : `后台运行 · ${runs.total}`;

  return (
    <ContextSection heading_ref={heading} is_open={is_open} on_toggle={on_toggle} title={title}>
      {notice && <p className={styles.notice} role="status">{notice.message}</p>}
      {runs.error && <div className={styles.error} role="alert">
        <span>{runs.error}</span><button className={styles.text_button} onClick={() => void runs.load()} type="button">重试</button>
      </div>}
      {!has_rows && !runs.error && <EmptyRow>{runs.total === null || runs.loading ? "正在读取后台运行…" : "没有后台运行。"}</EmptyRow>}
      {has_rows && <ul aria-label="后台运行列表" className={styles.list}>
        {runs.rows.map((row) => {
          const pending = runs.stopping.has(row.background_id);
          const stop_error = runs.stop_errors.get(row.background_id);
          const cleanup_failed = row.status === "cleanup_unconfirmed";
          return <li className={styles.item} key={row.background_id}>
            <div className={styles.row}>
              <button className={styles.task} onClick={() => runs.open(row)} type="button" title={row.command_summary}>
                <i aria-hidden="true" data-status={row.status} />
                <span>
                  <strong>{row.command_summary}</strong>
                  <small title={row.run_id}>{cleanup_failed ? "回收未确认" : "运行中"} · Run {row.run_id.slice(-8)} · {formatElapsed(now - row.started_at_ms)}</small>
                </span>
              </button>
              <button className={styles.stop} disabled={pending} onClick={() => void runs.stop(row.background_id)} type="button">
                {pending ? "正在停止…" : cleanup_failed || stop_error ? "重试停止" : "停止"}
              </button>
            </div>
            {stop_error && <p className={styles.error} role="alert">{stop_error}</p>}
          </li>;
        })}
      </ul>}
      {(pages > 1 || runs.page > 0) && <nav aria-label="后台运行分页" className={styles.pagination}>
        <InlineIconButton disabled={runs.loading || runs.page === 0} icon="chevron-left" label="上一页后台运行" onClick={() => void runs.load(runs.page - 1)} />
        <span>{runs.page + 1} / {pages}</span>
        <InlineIconButton disabled={runs.loading || runs.page + 1 >= pages} icon="chevron-right" label="下一页后台运行" onClick={() => void runs.load(runs.page + 1)} />
      </nav>}
      {browsing && runs.selected && <BackgroundOutputDialog key={runs.selected.background_id}
        client={runs.client} session_id={runs.session_id} run={runs.selected} is_stale={is_stale} on_close={close} on_end={end} />}
    </ContextSection>
  );
});

function formatElapsed(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const minutes = Math.floor(seconds / 60);
  if (minutes >= 60) return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`;
  if (minutes > 0) return `${minutes} 分 ${seconds % 60} 秒`;
  return `${seconds} 秒`;
}
