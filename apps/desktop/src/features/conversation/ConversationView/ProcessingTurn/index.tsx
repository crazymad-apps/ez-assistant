import { useId, useLayoutEffect, useRef, useState } from 'react';
import type {
  AssistantMessageSnapshot,
  ChildTaskTreeItemSnapshot,
  MessageId,
  RunStatus,
  ToolCallId,
} from '@ez-assistant/protocol';
import { Collapse } from '../../../../components/Collapse';
import { Icon } from '../../../../components/Icon';
import type { LiveRunProjection, LiveToolSnapshot } from '../../../../stores/LiveExecutionStore';
import { AssistantSegmentView, LiveSegmentView } from '../ToolSegments';
import { ReasoningBox } from '../ReasoningBox';
import { ProcessToolGroup } from '../ProcessToolGroup';
import { formatDateTime, liveRunStatusLabel, runFailureMessage, runStatusLabel } from '../conversationRows';
import { ElapsedTime } from './ElapsedTime';
import { groupProcessEntries, isRunActive, presentTurn, type TurnEntry } from './presentation';
import shared from '../index.module.scss';
import styles from './index.module.scss';

export function ProcessingTurn(
  props: Readonly<{
    messages: readonly AssistantMessageSnapshot[];
    live_run: LiveRunProjection | null;
    child_tasks: readonly ChildTaskTreeItemSnapshot[];
    task?: { status: RunStatus; created_at_ms: number; started_at_ms?: number | null; finished_at_ms: number | null };
    awaiting_feedback?: boolean;
    awaiting_approval?: boolean;
    reveal_message_id?: string | null;
    on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
    onFork: (message_id: MessageId) => void;
    show_fork: boolean;
    onLiveToolClick: (tool: LiveToolSnapshot) => void;
    onToolClick: (message_id: MessageId, call_id: ToolCallId) => void;
  }>,
) {
  const last = props.messages.at(-1);
  const inherited = props.messages.some((message) => message.inherited);
  const partial = props.messages.some((message) => message.partial);
  const run = inherited ? null : props.live_run;
  const status = partial ? null : run?.status ?? props.task?.status ?? last?.status ?? null;
  const active = !inherited && isRunActive(status);
  const { process, reply } = presentTurn(props.messages, run, status);
  const newest_step = Math.max(0, ...process.map((entry) => entry.step));
  const reveal =
    props.reveal_message_id && process.some((entry) => entry.message_id === props.reveal_message_id)
      ? props.reveal_message_id
      : null;
  const [state, setState] = useState({ open: active || !!reveal, active, newest_step, reveal, reveal_key: 0 });
  const [reasoning_open, setReasoningOpen] = useState<Record<string, boolean>>({});
  // 只对新中间 Step、可靠终态和显式引用定位反应。相同 Step 的增量/提交不覆盖手动选择；
  // 记录最大 Step，加载更早分页不会被当成新输出。render 内校准可在父级定位 effect 前展开。
  let open = state.open;
  if (active !== state.active || newest_step > state.newest_step || reveal !== state.reveal) {
    if (reveal) open = true;
    else if (!active && state.active) open = false;
    else if (active && newest_step > state.newest_step) open = true;
    setState({
      open,
      active,
      newest_step: Math.max(newest_step, state.newest_step),
      reveal,
      reveal_key: state.reveal_key + (reveal && reveal !== state.reveal ? 1 : 0),
    });
  }
  const id = useId();
  const header = useRef<HTMLButtonElement>(null);
  const content = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    if (!open && content.current?.contains(document.activeElement)) header.current?.focus();
  }, [open]);
  const clock = {
    status: status ?? 'completed',
    created_at_ms: run?.created_at_ms ?? props.task?.created_at_ms ?? last?.run_created_at_ms ?? null,
    started_at_ms: run?.started_at_ms ?? props.task?.started_at_ms ?? last?.run_started_at_ms ?? null,
    finished_at_ms: run?.finished_at_ms ?? props.task?.finished_at_ms ?? last?.finished_at_ms ?? null,
  };
  let label = '历史记录';
  if (partial) label = '截至分支点';
  else if (!inherited && props.awaiting_approval) label = '等待审批';
  else if (!inherited && props.awaiting_feedback) label = '等待回复';
  else if (run) label = liveRunStatusLabel(run);
  else if (status) label = runStatusLabel(status);
  if (inherited) label = `继承 · ${label}`;
  let error: string | null = null;
  if (!inherited && status === 'failed')
    error = runFailureMessage(run?.model_failure_kind ?? null, run?.error_code ?? null, run?.error_message ?? null);
  else if (!inherited && status === 'interrupted') error = '本轮已中断，请检查已有结果后继续。';
  const renderEntry = (entry: TurnEntry) => (
    <div key={entry.key} data-message-id={entry.message_id ?? undefined}>
      {entry.live ? (
        <LiveSegmentView
          child_tasks={props.child_tasks}
          state_key={entry.key}
          segment={entry.segment}
          is_reasoning_open={false}
          on_reasoning_toggle={() => {}}
          on_child_open={props.on_child_open}
          on_tool_click={props.onLiveToolClick}
        />
      ) : (
        <AssistantSegmentView
          child_tasks={props.child_tasks}
          message_id={entry.message_id!}
          segment={entry.segment}
          is_reasoning_open={false}
          on_reasoning_toggle={() => {}}
          on_child_open={props.on_child_open}
          on_tool_click={props.onToolClick}
        />
      )}
    </div>
  );
  const groups = groupProcessEntries(process);
  return (
    <article
      className={shared.assistant_message}
      data-processing-turn="true"
      data-quote-root="true"
      data-quote-role="assistant"
    >
      <button
        ref={header}
        className={styles.header}
        aria-label="处理过程"
        aria-controls={id}
        aria-expanded={open}
        onClick={() => setState({ ...state, open: !open })}
        type="button"
      >
        <span className={styles.title}>处理过程</span>
        <span role="status" className={styles.status} data-error={!!error || status === 'cancelled' || undefined}>
          {label}
        </span>
        {!partial && <span className={styles.elapsed}>
          <ElapsedTime run={clock} />
        </span>}
      </button>
      {/* 显式引用定位需在父级 layout effect 前拿到正文 DOM；仅该动作重新挂载 Collapse，正常增量保持身份。 */}
      <Collapse key={state.reveal_key} open={open} id={id}>
        <div ref={content} className={styles.process}>
          {groups.map((group) => {
            const first = group[0]!;
            if (first.segment.type === 'tool_group')
              return (
                <ProcessToolGroup
                  key={first.key}
                  tools={group.flatMap((entry) =>
                    entry.segment.type === 'tool_group'
                      ? entry.segment.tools.map((tool) => ({ tool_name: tool.tool_name, is_mcp: !!tool.mcp_identity || tool.input?.value.type === 'mcp' }))
                      : [],
                  )}
                >
                  {group.map(renderEntry)}
                </ProcessToolGroup>
              );
            if (first.segment.type === 'reasoning') {
              const reasoning = group.flatMap((entry) => (entry.segment.type === 'reasoning' ? [entry.segment] : []));
              const summary = reasoning.filter((part) => part.source === 'summary');
              return (
                <div key={first.key} data-message-id={first.message_id ?? undefined}>
                  <ReasoningBox
                    state_key={first.key}
                    text={(summary.length ? summary : reasoning).map((part) => part.text).join('\n')}
                    summary={summary.length > 0}
                    is_open={reasoning_open[first.key] ?? false}
                    on_toggle={() => setReasoningOpen((current) => ({ ...current, [first.key]: !current[first.key] }))}
                  />
                </div>
              );
            }
            return renderEntry(first);
          })}
          {process.length === 0 && active && reply.length === 0 && <p className={shared.waiting_text}>正在准备…</p>}
        </div>
      </Collapse>
      {error && (
        <p className={shared.run_error} role="alert">
          {error}
        </p>
      )}
      <div className={styles.reply}>{reply.map(renderEntry)}</div>
      {!active && last && (
        <div className={shared.message_actions}>
          <button
            aria-label="复制助手正文"
            onClick={() =>
              void navigator.clipboard?.writeText(
                props.messages
                  .flatMap((message) => message.segments)
                  .filter((segment) => segment.type === 'text')
                  .map((segment) => segment.text)
                  .join('\n\n'),
              )
            }
            type="button"
          >
            <Icon name="copy" size={15} />
          </button>
          {props.show_fork && (
            <button
              aria-label="从此消息分叉"
              disabled={!last.can_fork}
              onClick={() => props.onFork(last.message_id)}
              type="button"
            >
              <Icon name="fork" size={15} />
            </button>
          )}
          {!partial && <time>{formatDateTime(clock.finished_at_ms)}</time>}
        </div>
      )}
    </article>
  );
}
