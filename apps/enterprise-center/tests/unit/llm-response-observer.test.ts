import { expect, it } from 'vitest';
import { ResponseObserver } from '../../dist/llm/response-observer.js';
import { UsageObserver } from '../../dist/llm/usage.js';

const completed = {
  type: 'response.completed',
  response: { id: 'r', status: 'completed', output: [], usage: { input_tokens: 2, output_tokens: 1, total_tokens: 3 } },
};
const event = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`;

it('换行、BOM、UTF-8 任意切分得到相同观察和 usage', () => {
  for (const newline of ['\n', '\r\n', '\r'])
    for (const width of [1, 7, 1024]) {
      const usage = new UsageObserver('open_ai_responses', 'openai');
      const observer = new ResponseObserver({ protocol: 'open_ai_responses', kind: 'proxy' }, usage);
      observer.headers({ 'content-type': 'text/event-stream' });
      const body = Buffer.from(
        (
          '\uFEFF: 心跳\n\ndata: {"type":"response.output_text.delta","delta":"你好"}\n\n' + event(completed)
        ).replaceAll('\n', newline),
      );
      for (let i = 0; i < body.length; i += width) observer.chunk(body.subarray(i, i + width));
      expect(observer.result('forwarded', null)).toEqual({ state: 'normal', reason: null });
      expect(usage.result('forwarded', observer.frames.reason)).toMatchObject({
        state: 'available',
        total_tokens: '3',
      });
      observer.release();
    }
});

it('错误、坏帧、冲突与残片不能被正常终态覆盖', () => {
  const cases = [
    [
      event({ type: 'response.failed', response: { id: 'r', status: 'failed', output: [] } }) + event(completed),
      'anomalous',
    ],
    ['data: broken\n\n' + event(completed), 'anomalous'],
    [event(completed) + 'data: unfinished', 'unconfirmed'],
    [event(completed) + event({ type: 'response.output_text.delta', delta: 'late' }), 'anomalous'],
    [event({ type: 'response.created', response: { id: 'different' } }) + event(completed), 'anomalous'],
  ];
  for (const [body, state] of cases) {
    const observer = new ResponseObserver({ protocol: 'open_ai_responses', kind: 'proxy' });
    observer.headers({ 'content-type': 'text/event-stream' });
    observer.chunk(Buffer.from(body!));
    expect(observer.result('forwarded', null).state).toBe(state);
    observer.release();
  }
});

it('超限只跳过整帧，共享预算耗尽后释放', () => {
  const observer = new ResponseObserver({ protocol: 'open_ai_responses', kind: 'proxy' });
  observer.headers({ 'content-type': 'text/event-stream' });
  observer.chunk(Buffer.from('data: ' + 'x'.repeat(1024 * 1024 + 10) + event(completed) + '\n\n' + event(completed)));
  expect(observer.result('forwarded', null)).toEqual({ state: 'unconfirmed', reason: 'parser_limit' });
  observer.release();
  const held = Array.from({ length: 3 }, () => new ResponseObserver({ protocol: 'open_ai_responses', kind: 'proxy' }));
  for (const item of held) {
    item.headers({ 'content-type': 'application/json' });
    item.chunk(Buffer.alloc(8 * 1024 * 1024, 32));
  }
  expect(held[2]!.result('forwarded', null)).toMatchObject({ state: 'unconfirmed', reason: 'parser_budget' });
  held.forEach((item) => item.release());
});

it('Chat 所有 choice 完成且 DONE 才正常，受限输出和缺终态分别表达', () => {
  for (const [finish, done, state] of [
    ['stop', true, 'normal'],
    ['length', true, 'anomalous'],
    ['stop', false, 'unconfirmed'],
  ] as const) {
    const observer = new ResponseObserver({ protocol: 'open_ai_chat_completions', kind: 'proxy' });
    observer.headers({ 'content-type': 'text/event-stream' });
    observer.chunk(
      Buffer.from(
        event({ id: 'c', choices: [{ index: 0, delta: { content: 'text' }, finish_reason: finish }] }) +
          (done ? 'data: [DONE]\n\n' : ''),
      ),
    );
    expect(observer.result('forwarded', null).state).toBe(state);
    observer.release();
  }
});

it('JSON、compact、计量和压缩字节各自判断，不把合法 JSON 直接算正常', () => {
  for (const [kind, value, state] of [
    ['proxy', { status: 'completed', id: 'r', output: [] }, 'normal'],
    ['proxy', { choices: [] }, 'unconfirmed'],
    ['proxy', { error: { message: 'private' } }, 'anomalous'],
    [
      'context_compaction',
      { object: 'response.compaction', id: 'r', output: [{ type: 'compaction', encrypted_content: 'encrypted' }] },
      'normal',
    ],
    ['input_tokens', { input_tokens: 0 }, 'normal'],
  ] as const) {
    const observer = new ResponseObserver({ protocol: 'open_ai_responses', kind });
    observer.headers({ 'content-type': 'application/json' });
    observer.chunk(Buffer.from(JSON.stringify(value)));
    expect(observer.result('forwarded', null).state).toBe(state);
    observer.release();
  }
  const observer = new ResponseObserver({ protocol: 'open_ai_responses', kind: 'proxy' });
  observer.headers({ 'content-type': 'application/json', 'content-encoding': 'gzip' });
  observer.chunk(Buffer.from([0xff]));
  expect(observer.result('forwarded', null)).toEqual({ state: 'unconfirmed', reason: 'unsupported_encoding' });
});
