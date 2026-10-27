import { render } from "../support/renderWithAppearance";
import { act, cleanup, fireEvent, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { AssistantMessageSnapshot, AssistantSegment } from '@ez-assistant/protocol';
import { RootStore } from '../../src/stores/RootStore';
import { RootStoreProvider } from '../../src/stores/RootStoreContext';
import { emptyRun, hostClockAnchor } from '../../src/stores/liveExecutionProjection';
import type { LiveRunProjection } from '../../src/stores/LiveExecutionStore';
import { ProcessingTurn } from '../../src/features/conversation/ConversationView/ProcessingTurn';
import {
  attachLiveTurn,
  groupConversationTurns,
} from '../../src/features/conversation/ConversationView/conversationRows';
import {
  groupProcessEntries,
  presentTurn,
} from '../../src/features/conversation/ConversationView/ProcessingTurn/presentation';

const noop = () => {};
const options = { child_tasks: [], onFork: noop, show_fork: true, onLiveToolClick: noop, onToolClick: noop };
function message(
  id: string,
  step: number,
  segments: AssistantSegment[],
  status: AssistantMessageSnapshot['status'] = 'running',
): AssistantMessageSnapshot {
  return {
    inherited: false,
    partial: false,
    message_id: id,
    run_id: 'r',
    step,
    attempt: 1,
    segments,
    status,
    created_at_ms: null,
    run_created_at_ms: 1000,
    run_started_at_ms: 1000,
    finished_at_ms: status === 'completed' ? 9000 : null,
    usage: null,
    can_fork: true,
    fork_point: id,
    feedback: null,
  };
}
function live(): LiveRunProjection {
  return {
    ...emptyRun('s', 'r', 1000),
    started_at_ms: 1000,
    status: 'running',
    active_step: 1,
    steps: [{ step: 1, segments: [{ type: 'reasoning', source: 'raw', part_id: 'thought', text: '检查资料' }] }],
  };
}
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe('processing turn', () => {
  it('renders inherited partial history without future status, full duration or a live tail', () => {
    const store = new RootStore();
    const inherited = {
      ...message('retained', 1, [{ type: 'text', part_id: 'prefix', text: '可靠前缀' }], 'completed'),
      inherited: true,
      partial: true,
    };
    const rows = attachLiveTurn(groupConversationTurns([{ type: 'assistant', ...inherited }]), live(), null);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.type === 'assistant_turn' && rows[0].live_run).toBeUndefined();
    const mounted = render(<RootStoreProvider store={store}>
      <ProcessingTurn {...options} messages={[inherited]} live_run={live()} awaiting_approval />
    </RootStoreProvider>);
    expect(screen.getByRole('status')).toHaveTextContent('继承 · 截至分支点');
    expect(screen.queryByLabelText('处理耗时')).not.toBeInTheDocument();
    expect(screen.getByText('可靠前缀')).toBeVisible();
    expect(screen.queryByText('检查资料')).not.toBeInTheDocument();
    expect(screen.queryByText('等待审批')).not.toBeInTheDocument();
    mounted.rerender(<RootStoreProvider store={store}>
      <ProcessingTurn {...options} messages={[{ ...inherited, partial: false }]} live_run={null} />
    </RootStoreProvider>);
    expect(screen.getByRole('status')).toHaveTextContent('继承 · 已完成');
    store.dispose();
  });

  it.each(['same-message', 'later-tool-message'] as const)(
    'keeps the last body visible after tool-ended completion and history reload: %s', async (placement) => {
      const store = new RootStore();
      const tool: AssistantSegment = { type: 'tool_group', tools: [{
        call_id: 'question', tool_name: 'request_user_input', status: 'completed', summary: null,
        input: { value: { type: 'unavailable' }, truncated: false, redacted: false },
      }] };
      const text: AssistantSegment = { type: 'text', part_id: 'delivery', text: '已生成 10 米乘 10 米库房平面图。' };
      const messages = [
        message('progress', 1, [{ type: 'text', part_id: 'p', text: '我先读取图纸。' }, tool], 'completed'),
        message('delivery', 2, placement === 'same-message' ? [text, tool] : [text], 'completed'),
        ...(placement === 'later-tool-message' ? [message('question', 3, [tool], 'completed')] : []),
      ];
      const view = (run: LiveRunProjection | null) => <RootStoreProvider store={store}>
        <ProcessingTurn {...options} messages={messages} live_run={run} />
      </RootStoreProvider>;
      const mounted = render(view({ ...live(), steps: [] }));
      fireEvent.click(screen.getByRole('button', { name: '处理过程' }));
      await vi.waitFor(() => expect(screen.queryByText(text.text)).not.toBeInTheDocument());
      mounted.rerender(view({ ...live(), status: 'completed', steps: [], finished_at_ms: 9000 }));
      expect(screen.getByRole('button', { name: '处理过程' })).toHaveAttribute('aria-expanded', 'false');
      expect(screen.getByText(text.text)).toBeVisible();
      expect(screen.queryByText('我先读取图纸。')).not.toBeInTheDocument();
      mounted.unmount();
      render(view(null));
      expect(screen.getByText(text.text)).toBeVisible();
      expect(screen.getByText(text.text).closest('[data-message-id]')).toHaveAttribute('data-message-id', 'delivery');
      fireEvent.click(screen.getByRole('button', { name: '处理过程' }));
      expect(screen.getAllByText(text.text)).toHaveLength(1);
      expect(screen.getByText('我先读取图纸。')).toBeVisible();
      expect(screen.getAllByRole('button', { name: /工具调用：/ }).length).toBeGreaterThan(0);
      store.dispose();
    },
  );

  it('keeps active tool steps inside the process and does not invent a reply for tool-only history', () => {
    const tool: AssistantSegment = { type: 'tool_group', tools: [] };
    const messages = [message('m', 1, [{ type: 'text', part_id: 'p', text: '仍在读取' }, tool], 'completed')];
    expect(presentTurn(messages, null, 'running').reply).toHaveLength(0);
    expect(presentTurn(messages, null, 'completed').reply).toHaveLength(1);
    expect(presentTurn([message('m', 1, [tool], 'completed')], null).reply).toHaveLength(0);
  });

  it('keeps manual collapse across deltas and commit, opens for a new intermediate step, freezes on completion', () => {
    const store = new RootStore();
    const view = (messages: AssistantMessageSnapshot[], run: LiveRunProjection | null) => (
      <RootStoreProvider store={store}>
        <ProcessingTurn {...options} messages={messages} live_run={run} />
      </RootStoreProvider>
    );
    const { rerender } = render(view([], live()));
    const header = screen.getByRole('button', { name: '处理过程' });
    expect(header).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(header);
    rerender(
      view([], {
        ...live(),
        steps: [
          { step: 1, segments: [{ type: 'reasoning', source: 'raw', part_id: 'thought', text: '检查资料，继续分析' }] },
        ],
      }),
    );
    expect(header).toHaveAttribute('aria-expanded', 'false');
    const committed = message('m1', 1, [{ type: 'reasoning', part_id: 'thought', text: '检查资料，继续分析' }]);
    rerender(view([committed], { ...live(), steps: [] }));
    expect(header).toHaveAttribute('aria-expanded', 'false');
    rerender(
      view([committed], {
        ...live(),
        active_step: 2,
        steps: [{ step: 2, segments: [{ type: 'reasoning', source: 'raw', part_id: 'next', text: '下一步' }] }],
      }),
    );
    expect(header).toHaveAttribute('aria-expanded', 'true');
    const final = message('m2', 2, [{ type: 'text', part_id: 'answer', text: '最终回复始终可见' }], 'completed');
    rerender(view([{ ...committed, status: 'completed' }, final], null));
    expect(header).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByText('最终回复始终可见')).toBeVisible();
    expect(screen.getByLabelText('处理耗时')).toHaveTextContent('8秒');
  });

  it('prefers provider summary within reasoning, without creating a second expandable copy', () => {
    const store = new RootStore();
    const run = {
      ...live(),
      steps: [
        {
          step: 1,
          segments: [
            { type: 'reasoning' as const, source: 'raw' as const, part_id: 'raw', text: '不重复显示的原文' },
            {
              type: 'reasoning' as const,
              source: 'summary' as const,
              part_id: 'summary',
              text: '**已经找到两个关键差异**，检查 `mesh`',
            },
          ],
        },
      ],
    };
    render(
      <RootStoreProvider store={store}>
        <ProcessingTurn {...options} messages={[]} live_run={run} />
      </RootStoreProvider>,
    );
    expect(screen.getByLabelText('思考摘要')).toHaveTextContent(/^已经找到两个关键差异，检查 mesh$/);
    expect(screen.getByText('已经找到两个关键差异')).toHaveAttribute('data-streamdown', 'strong');
    expect(screen.getByText('mesh').closest('code')).not.toBeNull();
    expect(screen.queryByRole('button', { name: '思考过程' })).not.toBeInTheDocument();
    expect(screen.queryByText('不重复显示的原文')).not.toBeInTheDocument();
  });

  it('groups only adjacent tools across messages and preserves each detail target', () => {
    const store = new RootStore();
    const onToolClick = vi.fn();
    const tool = (id: string): AssistantSegment => ({
      type: 'tool_group',
      tools: [
        {
          call_id: id,
          tool_name: 'read_file',
          status: 'completed',
          summary: null,
          input: { value: { type: 'unavailable' }, truncated: false, redacted: false },
        },
      ],
    });
    const messages = [
      message('m1', 1, [tool('a')]),
      message('m2', 2, [tool('b')]),
      message('m3', 3, [{ type: 'text', part_id: 'progress', text: '查阅结果' }, tool('c')]),
    ];
    expect(groupProcessEntries(presentTurn(messages, live()).process).map((group) => group.length)).toEqual([
      2, 1, 1, 1,
    ]);
    render(
      <RootStoreProvider store={store}>
        <ProcessingTurn
          {...options}
          onToolClick={onToolClick}
          messages={messages}
          live_run={{ ...live(), steps: [] }}
        />
      </RootStoreProvider>,
    );
    fireEvent.click(screen.getAllByRole('button', { name: '工具调用：读取文件' })[0]!);
    fireEvent.click(screen.getAllByRole('button', { name: /read file完成/ })[1]!);
    expect(onToolClick).toHaveBeenCalledWith('m2', 'b');
    expect(screen.getAllByRole('button', { name: '工具调用：读取文件' })[1]!).toHaveAttribute('aria-expanded', 'false');
  });

  it('uses stable row identities for live-to-history handoff and separates compaction boundaries', () => {
    const first = attachLiveTurn([], live(), null)[0]!;
    const committed = message('m1', 1, [{ type: 'text', part_id: 'p', text: '正文' }]);
    const next = attachLiveTurn(groupConversationTurns([{ type: 'assistant', ...committed }]), live(), null)[0]!;
    expect(first.type === 'assistant_turn' && first.key).toBe(next.type === 'assistant_turn' && next.key);
    const rows = attachLiveTurn(
      groupConversationTurns([
        { type: 'assistant', ...committed },
        {
          type: 'user',
          inherited: false,
          message_id: 'u',
          input_id: null,
          text: '中断边界',
          created_at_ms: 0,
          attachment_ids: [],
          quotes: [],
          source: { type: 'user' },
        },
        { type: 'assistant', ...committed, message_id: 'm2' },
      ]),
      live(),
      null,
    );
    expect(rows[0]?.type === 'assistant_turn' && rows[0].live_run).toBeUndefined();
    expect(rows[2]?.type === 'assistant_turn' && rows[2].live_run).toBeTruthy();
    expect(rows[0]?.type === 'assistant_turn' && rows[0].key).not.toBe(
      rows[2]?.type === 'assistant_turn' && rows[2].key,
    );
  });

  it('does not mount an execution timer before starting or for a cancelled queued input', () => {
    const store = new RootStore();
    const interval = vi.spyOn(window, 'setInterval');
    const view = (run: LiveRunProjection) => <RootStoreProvider store={store}>
      <ProcessingTurn {...options} messages={[]} live_run={run} />
    </RootStoreProvider>;
    const accepted = emptyRun('s', 'r', 1);
    const { rerender } = render(view(accepted));
    expect(screen.getByRole('status')).toHaveTextContent('等待执行');
    expect(screen.queryByLabelText('处理耗时')).not.toBeInTheDocument();
    expect(interval).not.toHaveBeenCalled();
    rerender(view({ ...accepted, status: 'cancelled', finished_at_ms: 1_000_000 }));
    expect(screen.queryByLabelText('处理耗时')).not.toBeInTheDocument();
    expect(interval).not.toHaveBeenCalled();
  });

  it('advances while waiting, stops on terminal feedback, and keeps unknown time unavailable', () => {
    vi.useFakeTimers();
    const now = vi.spyOn(performance, 'now').mockReturnValue(100);
    const store = new RootStore();
    store.live_execution.clock_anchor = hostClockAnchor(5000, 100);
    const view = (run: LiveRunProjection) => (
      <RootStoreProvider store={store}>
        <ProcessingTurn {...options} messages={[]} live_run={run} awaiting_feedback />
      </RootStoreProvider>
    );
    const { rerender } = render(view(live()));
    expect(screen.getByRole('status')).toHaveTextContent('等待回复');
    expect(screen.getByLabelText('处理耗时')).toHaveTextContent('4秒');
    act(() => {
      now.mockReturnValue(2100);
      vi.advanceTimersByTime(2000);
    });
    expect(screen.getByLabelText('处理耗时')).toHaveTextContent('6秒');
    rerender(view({ ...live(), status: 'completed', finished_at_ms: 7000 }));
    act(() => {
      now.mockReturnValue(90100);
      vi.advanceTimersByTime(90000);
    });
    expect(screen.getByLabelText('处理耗时')).toHaveTextContent('6秒');
    rerender(view({ ...live(), started_at_ms: null }));
    expect(screen.queryByLabelText('处理耗时')).not.toBeInTheDocument();
  });

  it('reveals a historical intermediate message before positioning its quote and keeps errors outside collapse', () => {
    const store = new RootStore();
    const messages = [
      message('middle', 1, [{ type: 'text', part_id: 'p', text: '引用的中间说明' }], 'failed'),
      message('final', 2, [{ type: 'text', part_id: 'p2', text: '部分结果' }], 'failed'),
    ];
    const view = (reveal?: string) => (
      <RootStoreProvider store={store}>
        <ProcessingTurn {...options} messages={messages} live_run={null} reveal_message_id={reveal} />
      </RootStoreProvider>
    );
    const { rerender } = render(view());
    expect(screen.queryByText('引用的中间说明')).not.toBeInTheDocument();
    expect(screen.getByRole('alert')).toBeVisible();
    rerender(view('middle'));
    expect(screen.getByText('引用的中间说明')).toBeVisible();
    rerender(view());
    expect(screen.getByRole('button', { name: '处理过程' })).toHaveAttribute('aria-expanded', 'true');
  });
});

it('does not append an empty main turn for an unstarted Run', () => {
  expect(attachLiveTurn([], emptyRun('s', 'queued', 1), null)).toEqual([]);
});
