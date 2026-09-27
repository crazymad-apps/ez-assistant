import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { emptyRun } from '../../src/stores/liveExecutionProjection';
import { LiveAssistantMessage } from '../../src/features/conversation/ConversationView/MessageViews';
import { runFailureMessage } from '../../src/features/conversation/ConversationView/conversationRows';

afterEach(cleanup);

describe('model request feedback', () => {
  it('shows waiting, reconnecting and generation in the existing status row', () => {
    const run = { ...emptyRun('session-1', 'run-1', 0), status: 'running' as const };
    const { rerender } = render(<LiveAssistantMessage onToolClick={() => {}} run={{ ...run, model_request: { type: 'retry_waiting', next_attempt: 2, delay_ms: 1000 } }} />);
    expect(screen.getByRole('status')).toHaveTextContent('等待 1 秒后进行第 2 次尝试');
    rerender(<LiveAssistantMessage onToolClick={() => {}} run={{ ...run, model_request: { type: 'requesting', attempt: 2 } }} />);
    expect(screen.getByRole('status')).toHaveTextContent('正在重新连接模型 · 第 2 次尝试');
    rerender(<LiveAssistantMessage onToolClick={() => {}} run={run} />);
    expect(screen.getByRole('status')).toHaveTextContent('正在生成');
    rerender(<LiveAssistantMessage awaiting_feedback onToolClick={() => {}} run={run} />);
    expect(screen.getByRole('status')).toHaveTextContent('等待回复');
  });

  it('explains exhaustion and stream interruption from persisted safe diagnostics', () => {
    expect(runFailureMessage(null, 'model_execution_failed', 'model execution failed before stream establishment (kind=service_unavailable, attempts=6, retries=5, output_observed=false)')).toContain('已尝试 6 次，未能恢复');
    expect(runFailureMessage(null, 'model_execution_failed', 'model execution failed after stream establishment (kind=stream_interrupted, attempts=1, retries=0, output_observed=true)')).toContain('已停止自动重试');
    expect(runFailureMessage(null, 'model_execution_failed', 'private API key')).not.toContain('private');
  });
});
