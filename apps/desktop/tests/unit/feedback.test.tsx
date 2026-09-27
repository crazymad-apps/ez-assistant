import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { FeedbackRequestSnapshot, RuntimeEventEnvelope } from '@ez-assistant/protocol';
import { FeedbackStore } from '../../src/features/feedback/FeedbackStore';
import { FeedbackPanel } from '../../src/features/feedback/FeedbackPanel';
import type { RuntimeClient } from '../../src/runtime-client/RuntimeClient';

function request(id = 'feedback-1', session_id = 'session-1'): FeedbackRequestSnapshot {
  return { request_id: id, session_id, run_id: 'run-1', tool_call_id: 'call-1', mode: 'wait', status: 'pending', answers: null,
    questions: [
      { question: '选择格式，也可以自己填写', header: '格式', multiple: false, options: [{ label: 'PDF', description: '便于阅读' }, { label: '文档', description: null }] },
      { question: '需要哪些内容？', header: '内容', multiple: true, options: [{ label: '说明', description: null }, { label: '清单', description: null }] },
      { question: '还需要什么？', header: null, multiple: false, options: [] },
    ] };
}
function fixture() {
  let scope = 'host-user-1';
  const command = vi.fn();
  const client = { command } as unknown as RuntimeClient;
  const store = new FeedbackStore({ client: () => client, scope: () => scope });
  store.setScope(scope);
  return { store, command, changeScope(next: string) { scope = next; store.setScope(next); } };
}
function event(snapshot: FeedbackRequestSnapshot): RuntimeEventEnvelope {
  return { sequence: 1, emitted_at_ms: 1, event: { type: 'feedback_published', request: snapshot } };
}
afterEach(() => cleanup());

describe('问答面板与草稿', () => {
  it('无预选，题目切换保留选择和补充，整组完成后才提交', async () => {
    const { store, command } = fixture(); const question = request();
    render(<FeedbackPanel request={question} store={store} />);
    expect(screen.getByRole('radio', { name: /PDF/ })).not.toBeChecked();
    expect(screen.getByRole('button', { name: '下一题' })).toBeDisabled();
    fireEvent.click(screen.getByRole('radio', { name: /PDF/ }));
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '保留源文件' } });
    fireEvent.click(screen.getByRole('button', { name: '内容' }));
    fireEvent.click(screen.getByRole('checkbox', { name: '说明' }));
    fireEvent.click(screen.getByRole('checkbox', { name: '清单' }));
    fireEvent.click(screen.getByRole('button', { name: /格式/ }));
    expect(screen.getByRole('radio', { name: /PDF/ })).toBeChecked();
    expect(screen.getByRole('textbox')).toHaveValue('保留源文件');
    fireEvent.click(screen.getByRole('button', { name: '问题 3' }));
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '中文回答' } });
    command.mockResolvedValue({ type: 'answer_feedback', payload: { ...question, status: 'answered', answers: store.draft(question).answers } });
    fireEvent.click(screen.getByRole('button', { name: '回复并继续' }));
    await waitFor(() => expect(command).toHaveBeenCalledTimes(1));
    expect(command.mock.calls[0][0].payload.answers).toEqual([
      { selected_options: [0], text: '保留源文件' }, { selected_options: [0, 1], text: '' }, { selected_options: [], text: '中文回答' },
    ]);
  });

  it('下一题和快捷键只导航，缺题时引导补填，最后才统一提交', async () => {
    const { store, command } = fixture(); const question = request();
    render(<FeedbackPanel request={question} store={store} />);
    expect(screen.getByRole('radio', { name: /PDF/ })).toHaveFocus();
    fireEvent.click(screen.getByRole('radio', { name: /PDF/ }));
    expect(screen.getByRole('button', { name: '下一题' })).toBeEnabled();
    fireEvent.click(screen.getByRole('button', { name: '下一题' }));
    expect(screen.getByRole('checkbox', { name: '说明' })).toHaveFocus();
    expect(command).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: '问题 3' }));
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '最后一题' } });
    fireEvent.click(screen.getByRole('button', { name: '前往未答题' }));
    expect(screen.getByRole('checkbox', { name: '说明' })).toHaveFocus();
    fireEvent.click(screen.getByRole('checkbox', { name: '说明' }));
    fireEvent.keyDown(screen.getByRole('textbox'), { key: 'Enter', ctrlKey: true, repeat: true });
    expect(screen.getByRole('checkbox', { name: '说明' })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole('textbox'), { key: 'Enter', ctrlKey: true });
    expect(screen.getByRole('textbox')).toHaveValue('最后一题');
    expect(command).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: '上一题' }));
    expect(screen.getByRole('checkbox', { name: '说明' })).toBeChecked();
    fireEvent.click(screen.getByRole('button', { name: '下一题' }));
    command.mockResolvedValue({ payload: { ...question, status: 'answered', answers: store.draft(question).answers } });
    fireEvent.click(screen.getByRole('button', { name: '回复并继续' }));
    await waitFor(() => expect(command).toHaveBeenCalledTimes(1));
  });

  it('选项与补充关系明确，清除选择保留输入，纯文本也能完成当前题', () => {
    const { store } = fixture(); const question = request();
    render(<FeedbackPanel request={question} store={store} />);
    fireEvent.click(screen.getByRole('radio', { name: /PDF/ }));
    const textbox = screen.getByRole('textbox', { name: '补充说明（可选）' });
    textbox.focus();
    fireEvent.change(textbox, { target: { value: '只保留文本答案' } });
    expect(textbox).toHaveFocus();
    fireEvent.click(screen.getByRole('button', { name: '清除选择' }));
    expect(screen.getByRole('radio', { name: /PDF/ })).not.toBeChecked();
    expect(screen.getByRole('textbox', { name: '你的回答' })).toHaveValue('只保留文本答案');
    expect(screen.getByRole('button', { name: '下一题' })).toBeEnabled();
  });

  it('输入法确认不会提交，失败保留填写，重复点击只发送一次', async () => {
    const { store, command } = fixture(); const question = { ...request(), questions: [request().questions[2]] };
    let reject!: (error: Error) => void;
    command.mockImplementation(() => new Promise((_, failed) => { reject = failed; }));
    render(<FeedbackPanel request={question} store={store} />);
    const textbox = screen.getByRole('textbox');
    fireEvent.change(textbox, { target: { value: '中文' } });
    fireEvent.compositionStart(textbox);
    fireEvent.keyDown(textbox, { key: 'Enter', ctrlKey: true, isComposing: true });
    fireEvent.compositionEnd(textbox);
    fireEvent.keyDown(textbox, { key: 'Enter', ctrlKey: true });
    expect(command).not.toHaveBeenCalled();
    fireEvent.keyUp(textbox, { key: 'Enter' });
    fireEvent.keyDown(textbox, { key: 'Enter', metaKey: true });
    fireEvent.click(screen.getByRole('button', { name: '提交中…' }));
    expect(command).toHaveBeenCalledTimes(1);
    await act(async () => reject(new Error('网络中断')));
    expect(screen.getByRole('alert')).toHaveTextContent('网络中断');
    expect(textbox).toHaveValue('中文');
    expect(screen.getByRole('button', { name: '回复并继续' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: '收起' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '打开待回答问题' })).not.toBeInTheDocument();
    fireEvent.keyDown(textbox, { key: 'Escape' });
    expect(screen.getByRole('region', { name: '等待你回答' })).toBeVisible();
    expect(command).toHaveBeenCalledTimes(1);
  });

  it('取消状态事件先到也保持阻塞，取消命令确认后才释放输入区', async () => {
    const { store, command } = fixture(); const question = request();
    store.ingest(event(question));
    store.setDraft(question, { answers: question.questions.map(() => ({ selected_options: [], text: '保留回答' })) });
    let finish!: (value: unknown) => void;
    command.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const cancelling = store.cancel(question);
    store.ingest(event({ ...question, status: 'cancelled' }));
    expect(store.waiting(question.session_id)?.request_id).toBe(question.request_id);
    expect(store.draft(question).answers[0].text).toBe('保留回答');
    render(<FeedbackPanel request={store.waiting(question.session_id)!} store={store} />);
    expect(screen.getByRole('region', { name: '等待你回答' })).toBeVisible();
    expect(screen.getByRole('button', { name: '取消中…' })).toBeDisabled();
    await act(async () => { finish({ payload: { ...question, status: 'cancelled' } }); await cancelling; });
    expect(store.waiting(question.session_id)).toBeUndefined();
  });

  it('跨会话保留草稿，切换 Runtime 用户清空，旧响应不串入', async () => {
    const { store, command, changeScope } = fixture(); const first = request(); const second = request('feedback-2', 'session-2');
    store.ingest(event(first)); store.ingest(event(second));
    store.setDraft(first, { answers: [{ selected_options: [], text: '第一份' }] });
    store.setDraft(second, { answers: [{ selected_options: [], text: '第二份' }] });
    expect(store.draft(first).answers[0].text).toBe('第一份');
    expect(store.draft(second).answers[0].text).toBe('第二份');
    let finish!: (value: unknown) => void;
    command.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const loading = store.refresh('session-1'); changeScope('host-user-2');
    finish({ payload: [first] }); await loading;
    expect(store.requests.size).toBe(0);
    expect(store.draft(first).answers[0].text).toBe('');
  });

  it('收到新状态后，旧快照不会覆盖回答', async () => {
    const { store, command } = fixture(); const question = request(); let finish!: (value: unknown) => void;
    command.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const loading = store.refresh(question.session_id);
    store.ingest(event({ ...question, status: 'answered' }));
    finish({ payload: [question] }); await loading;
    expect(store.requests.get(question.request_id)?.status).toBe('answered');
  });
});
