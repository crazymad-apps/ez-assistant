import { afterEach, describe, expect, it, vi } from 'vitest';
import { MIN_COMPATIBLE_VERSION, SOFTWARE_VERSION, type ConversationItem, type ConversationPage } from '@ez-assistant/protocol';
import { RuntimeClient } from '../../src/runtime-client/RuntimeClient';
import { notificationText, resolveNotificationContent } from '../../src/features/notifications/content';
import { notificationFromEvent, type SystemNotification } from '../../src/features/notifications/types';

const notification: SystemNotification = {
  target: { scope: 'scope', owner: { type: 'main_session', session_id: 'source' }, run_id: 'old-run', kind: 'turn' },
  title: '会话通知', body: '本轮已完成，暂无文字回复。',
};
const clients: RuntimeClient[] = [];

afterEach(() => {
  clients.splice(0).forEach((client) => client.dispose());
  vi.unstubAllGlobals();
});

function assistant(overrides: Partial<Extract<ConversationItem, { type: 'assistant' }>> = {}): ConversationItem {
  return { type: 'assistant', inherited: false, partial: false, message_id: 'answer', run_id: 'old-run', attempt: 1, step: 2,
    created_at_ms: null, finished_at_ms: 100, status: 'completed', segments: [
      { type: 'reasoning', part_id: 'reasoning', text: '不应出现在通知中的推理' },
      { type: 'text', part_id: 'body', text: '**旧轮回复**：已完成 [检查](https://example.test/private)' },
      { type: 'tool_group', tools: [] },
    ], usage: null, can_fork: true, fork_point: 'answer', feedback: null, ...overrides };
}

function fixture(options: { title?: string; session_id?: string; page?: Partial<ConversationPage>; items?: ConversationItem[]; failure?: string } = {}) {
  const client = new RuntimeClient({
    base_url: 'http://127.0.0.1:19000', access_token: 'fixture', instance_id: 'fixture', started_runtime: false,
    capabilities: { mode: 'personal', min_compatible_version: MIN_COMPATIBLE_VERSION, runtime_version: SOFTWARE_VERSION,
      max_command_bytes: 65536, max_attachment_bytes: null, sse: true, streaming_upload: true, features: [] },
  });
  clients.push(client);
  const requests: { type: string; payload: object }[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body)) as { request_id: string; command: { payload: { type: string; payload: object } } };
    const command = body.command.payload;
    requests.push(command);
    if (command.type === options.failure) throw new Error('private diagnostic');
    const page: ConversationPage = { owner: { type: 'main_session', session_id: 'source' }, generation: 1,
      items: options.items ?? [assistant()], previous_cursor: null, has_more: false, ...options.page };
    const payload = command.type === 'get_session'
      ? { session: { session_id: options.session_id ?? 'source', title: options.title ?? '来源会话' } }
      : { snapshot: { observed_sequence: 2, value: page }, anchor_message_id: 'answer' };
    return new Response(JSON.stringify({ request_id: body.request_id, result: { scope: 'runtime', payload: { type: command.type, payload } } }), { status: 200 });
  }));
  return { client, requests };
}

describe('通知可靠内容', () => {
  it('按会话、Run 和锚点读取末条助手正文，排除后继消息、推理与工具', async () => {
    const { client, requests } = fixture({ items: [assistant(), assistant({ message_id: 'later', run_id: 'later-run', segments: [{ type: 'text', part_id: 'later', text: '错误的后继回复' }] })] });
    expect(await resolveNotificationContent(client, notification)).toMatchObject({ title: '来源会话', body: '旧轮回复：已完成 检查' });
    expect(requests).toEqual([
      { type: 'get_session', payload: { session_id: 'source' } },
      { type: 'get_conversation_page_around_run', payload: { session_id: 'source', run_id: 'old-run', limit: 1 } },
    ]);
  });

  it.each(['session', 'owner', 'run', 'anchor'] as const)('拒绝 %s 归属不匹配的内容', async (mismatch) => {
    const { client } = fixture({
      session_id: mismatch === 'session' ? 'other' : undefined,
      page: mismatch === 'owner' ? { owner: { type: 'child_task', session_id: 'source', child_task_id: 'child' } } : undefined,
      items: [assistant({ run_id: mismatch === 'run' ? 'later-run' : 'old-run', message_id: mismatch === 'anchor' ? 'different' : 'answer' })],
    });
    expect(await resolveNotificationContent(client, notification)).toBeNull();
  });

  it('没有正文时使用事件兜底，不取更早文字或 reasoning', async () => {
    const { client } = fixture({ title: '  ', items: [assistant({ message_id: 'earlier' }), assistant({ segments: [{ type: 'reasoning', part_id: 'r', text: 'private' }] })] });
    expect(await resolveNotificationContent(client, notification)).toMatchObject({ title: '未命名会话', body: notification.body });
  });

  it('可靠消息定位不可用只保留状态文案，会话不可访问则不投递', async () => {
    const first = fixture({ failure: 'get_conversation_page_around_run' });
    expect(await resolveNotificationContent(first.client, notification)).toMatchObject({ title: '来源会话', body: notification.body });
    const second = fixture({ failure: 'get_session' });
    expect(await resolveNotificationContent(second.client, notification)).toBeNull();
  });

  it('非正常完成保留状态前缀，不显示原始异常', async () => {
    const { client } = fixture({ items: [assistant({ status: 'failed' })] });
    const failure = notificationFromEvent('scope', { sequence: 1, emitted_at_ms: 1,
      event: { type: 'run_finished', session_id: 'source', run_id: 'old-run', status: 'failed', error: { code: 'model_execution_failed', message: 'private provider error' } } })!;
    const content = await resolveNotificationContent(client, failure);
    expect(content?.body).toContain('本轮执行未完成');
    expect(content?.body).toContain('旧轮回复');
    expect(content?.body).not.toContain('private');
  });

  it('子任务审批和问答只查询来源父会话标题，保留事件正文', async () => {
    const { client, requests } = fixture();
    const approval: SystemNotification = { ...notification, target: { ...notification.target, kind: 'approval', approval_id: 'approval',
      owner: { type: 'child_task', session_id: 'source', child_task_id: 'child' } }, body: '需要审批：shell。点击查看并处理。' };
    expect(await resolveNotificationContent(client, approval)).toMatchObject({ title: '来源会话', body: approval.body });
    const feedback: SystemNotification = { ...notification, target: { ...notification.target, kind: 'feedback', request_id: 'question' }, body: '等待回复：使用 **什么格式**？' };
    expect(await resolveNotificationContent(client, feedback)).toMatchObject({ title: '来源会话', body: '等待回复：使用 什么格式？' });
    expect(requests.map((request) => request.type)).toEqual(['get_session', 'get_session']);
  });
});

describe('通知纯文本与长度', () => {
  it('保留正文可读内容，去掉 Markdown 装饰与资源目标，不执行 HTML', () => {
    const source = '# 标题\n\n- **完成** [报告](https://example.test/token)\n![图](file:///private.png)\n```ts\nconst file_name = "a";\n```\n<script>secret()</script><b>文本</b> &amp; &#x1F600;';
    expect(notificationText(source)).toBe('标题 完成 报告 图 const file_name = "a"; 文本 & 😀');
    expect(notificationText('删除 ~~旧值~~，保留 `file_name` 和 _强调_')).toBe('删除 旧值，保留 file_name 和 强调');
  });

  it('按 Unicode 码点限长，不切坏 emoji；标题和正文分别有界', () => {
    const text = '😀'.repeat(200);
    expect(Array.from(notificationText(text))).toHaveLength(180);
    expect(notificationText(text).endsWith('😀…')).toBe(true);
    expect(Array.from(notificationText(text, 64))).toHaveLength(64);
    expect(notificationText('\u202E正文\n\t内容\u0000')).toBe('正文 内容');
  });
});
