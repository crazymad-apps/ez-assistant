import { expect, it } from 'vitest';
import type { EntityManager } from 'typeorm';
import { UsageObserver } from '../../dist/llm/usage.js';
import { queryUsage } from '../../dist/llm/usage-query.js';

it('Chat SSE 在任意分块、流末 usage 和重复快照下只记录一次累计用量', () => {
  const observer = new UsageObserver('open_ai_chat_completions', 'deepseek');
  observer.headers({ 'content-type': 'text/event-stream; charset=utf-8' });
  const frame = 'data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_cache_hit_tokens":4}}\r\n\r\n';
  const body = Buffer.from(`${frame}${frame}data: [DONE]\r\n\r\n`);
  for (let offset = 0; offset < body.length; offset += 7) observer.chunk(body.subarray(offset, offset + 7));
  expect(observer.result('forwarded')).toMatchObject({
    state: 'available', input_tokens: '10', output_tokens: '5', total_tokens: '15', cached_input_tokens: '4',
  });
});

it('Responses JSON 在正文采集关闭时也可观察 usage，缺失与不完整不算零', () => {
  const observer = new UsageObserver('open_ai_responses', null);
  observer.headers({ 'content-type': 'application/json' });
  observer.chunk(Buffer.from('{"usage":{"input_tokens":6,"output_tokens":2,"total_tokens":8}}'));
  expect(observer.result('forwarded')).toMatchObject({ state: 'available', total_tokens: '8' });
  const missing = new UsageObserver('open_ai_responses', null);
  missing.headers({ 'content-type': 'application/json' });
  missing.chunk(Buffer.from('{"output":[]}'));
  expect(missing.result('forwarded')).toMatchObject({ state: 'unavailable', reason: 'not_reported', total_tokens: null });
});

it('Responses 流式终态和异常数值有明确状态，不让解析影响转发', () => {
  const observer = new UsageObserver('open_ai_responses', null);
  observer.headers({ 'content-type': 'text/event-stream' });
  observer.chunk(Buffer.from('event: response.completed\ndata: {"type":"response.completed","response":{"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}}\n\n'));
  expect(observer.result('forwarded')).toMatchObject({ state: 'available', total_tokens: '5' });
  const invalid = new UsageObserver('open_ai_responses', null);
  invalid.headers({ 'content-type': 'application/json' });
  invalid.chunk(Buffer.from('{"usage":{"input_tokens":3,"output_tokens":2,"total_tokens":4}}'));
  expect(invalid.result('forwarded')).toMatchObject({ state: 'partial', reason: 'invalid_usage', total_tokens: null });
});

it('无关 SSE 心跳和多行事件不污染终态 usage', () => {
  const observer = new UsageObserver('open_ai_responses', null);
  observer.headers({ 'content-type': 'text/event-stream' });
  const stream = 'data: ping\n\n: heartbeat\n\nevent: response.completed\ndata: {"type":"response.completed",\ndata: "response":{"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}\n\n';
  for (const byte of Buffer.from(stream)) observer.chunk(Buffer.from([byte]));
  expect(observer.result('forwarded')).toMatchObject({ state: 'available', total_tokens: '3' });
});

it('同一流中响应 ID 改变时不将最终用量标为完整', () => {
  const observer = new UsageObserver('open_ai_responses', null);
  observer.headers({ 'content-type': 'text/event-stream' });
  observer.chunk(Buffer.from('data: {"type":"response.created","response":{"id":"first"}}\n\n'));
  observer.chunk(Buffer.from('data: {"type":"response.completed","response":{"id":"second","usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}\n\n'));
  expect(observer.result('forwarded')).toMatchObject({ state: 'partial', reason: 'invalid_usage', total_tokens: '3' });
});

it('上海时区的月桶跨越短月时仍从每月一日开始', async () => {
  let reads = 0;
  const tx = {
    query: async () => {
      reads++;
      if (reads === 1) return [{ calls: '0' }];
      if (reads === 4) return [{ total: 0 }];
      if (reads === 5) return [{ index_retention_days: 90, usage_available_from: new Date('2026-01-01T00:00:00Z') }];
      return [];
    },
  };
  const manager = { transaction: async (_level: string, run: (value: typeof tx) => Promise<unknown>) => run(tx) } as unknown as EntityManager;
  const page = await queryUsage(manager, {
    from: '2026-01-31T16:00:00Z', to: '2026-04-30T16:00:00Z', granularity: 'month',
  });
  expect(page.buckets.map((bucket) => bucket.start)).toEqual([
    '2026-01-31T16:00:00.000Z', '2026-02-28T16:00:00.000Z', '2026-03-31T16:00:00.000Z',
  ]);
});
