import { expect, it } from 'vitest';
import { requestProjection } from '../../src/features/calls/BodyReader/request';
import { responseProjection } from '../../src/features/calls/BodyReader/response';
import { scanFrames } from '../../src/features/calls/BodyReader/frames';
const signal = () => new AbortController().signal;
const event = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`;
const delta = (text: string, output = 0) =>
  event({ type: 'response.output_text.delta', output_index: output, content_index: 0, delta: text });
const done = (text: string, output = 0) =>
  event({ type: 'response.output_text.done', output_index: output, content_index: 0, text });
const end = event({ type: 'response.completed', response: { id: 'r', status: 'completed', output: [] } });
const parse = (text: string) => responseProjection(text, 'text/event-stream', signal());

it('相邻聚合保留重复字，交错输出不移动；done 与终态不重复正文', async () => {
  const text =
    delta('哈') +
    delta('哈') +
    delta('B', 1) +
    delta('你好') +
    done('哈哈你好') +
    event({
      type: 'response.completed',
      response: {
        id: 'r',
        status: 'completed',
        output: [
          { type: 'message', content: [{ type: 'output_text', text: '哈哈你好' }] },
          { type: 'message', content: [{ type: 'output_text', text: 'B' }] },
        ],
      },
    });
  const p = await parse(text),
    content = p.blocks.filter((b) => b.key);
  expect(content.map((b) => b.text)).toEqual(['哈哈', 'B', '你好']);
  expect(content.map((b) => b.deltaCount)).toEqual([2, 1, 1]);
  expect(content[2]?.continuation).toBe(true);
  expect(content.every((b) => b.status === '内容已结束')).toBe(true);
  for (const block of content)
    for (const source of block.sources) expect(text.slice(source.start, source.end)).toContain('data:');
});
it('终态补足放在真实位置，冲突保留两份内容', async () => {
  const p = await parse(delta('A') + delta('B', 1) + done('AC') + done('AX') + end);
  expect(p.blocks.filter((b) => b.key).map((b) => b.text)).toEqual(['A', 'B', 'C']);
  expect(p.blocks.find((b) => b.conflict !== undefined)).toMatchObject({ text: 'AC', conflict: 'AX' });
});
it('缺少归属不误并，坏帧恢复，尾部残片与未完成单独表达', async () => {
  const p = await parse(
    delta('前') +
      event({ type: 'response.output_text.delta', delta: '未知' }) +
      'data: {bad}\n\n' +
      delta('后') +
      'data: {tail',
  );
  expect(p.blocks.filter((b) => b.key).map((b) => b.text)).toEqual(['前', '后']);
  expect(p.blocks.filter((b) => b.warning)).toHaveLength(3);
  expect(p.notices.join()).toContain('未找到完整结束信号');
});
it('多 choice、多工具按 index 归属，finish 不冒充工具执行', async () => {
  const chunk = (choices: unknown[]) => event({ choices });
  const p = await parse(
    chunk([
      {
        index: 0,
        delta: {
          tool_calls: [
            { index: 0, id: 'a', function: { name: 'read', arguments: '{' } },
            { index: 1, id: 'b', function: { name: 'search', arguments: '[' } },
          ],
        },
      },
    ]) +
      chunk([
        { index: 0, delta: { tool_calls: [{ index: 0, function: { arguments: '}' } }] } },
        { index: 1, delta: { content: '文字' }, finish_reason: 'length' },
      ]) +
      chunk([{ index: 0, delta: {}, finish_reason: 'tool_calls' }]) +
      'data: [DONE]\n\n',
  );
  expect(p.blocks.filter((b) => b.title === '工具调用请求').map((b) => b.text)).toEqual(['{', '[', '}']);
  expect(p.blocks.filter((b) => b.key).every((b) => b.status?.startsWith('生成结束'))).toBe(true);
  expect(p.blocks.some((b) => b.warning?.includes('length'))).toBe(true);
  expect(p.notices.join()).not.toContain('未完整结束');
});
it('指令与消息分离，多模态不加载，坏请求不猜测修复', async () => {
  const p = await requestProjection(
    JSON.stringify({
      instructions: '规则',
      previous_response_id: 'old',
      input: [
        {
          role: 'user',
          content: [
            { type: 'input_text', text: '看图' },
            { type: 'input_image', image_url: 'https://example.invalid/private.png' },
          ],
        },
        { type: 'function_call_output', call_id: 'missing', output: 'result' },
      ],
      tools: [{ name: 'read' }],
    }),
    signal(),
  );
  expect(p.blocks[0]?.title).toBe('系统提示词');
  expect(p.blocks[1]?.text).toContain('不自动加载');
  expect(p.blocks[1]?.text).not.toContain('https:');
  expect(p.blocks[2]?.status).toContain('未包含关联调用');
  expect(p.metadata).toContain('tools');
  expect((await requestProjection('{"input":[', signal())).blocks).toEqual([]);
});
it('CR/LF/CRLF、BOM、多行 data 和 UTF-8 逐字节切分还原相同内容', async () => {
  const raw = '\uFEFF: heartbeat\n\nevent: custom\ndata: {"text":\ndata: "你好"}\n\n' + delta('字') + end;
  const bytes = new TextEncoder().encode(raw),
    decoder = new TextDecoder('utf-8', { ignoreBOM: true });
  let assembled = '';
  for (const byte of bytes) assembled += decoder.decode(new Uint8Array([byte]), { stream: true });
  assembled += decoder.decode();
  const baseline = await parse(raw);
  expect(await parse(assembled)).toEqual(baseline);
  for (const newline of ['\r', '\r\n'])
    expect((await parse(raw.replaceAll('\n', newline))).blocks.map((b) => [b.title, b.text])).toEqual(
      baseline.blocks.map((b) => [b.title, b.text]),
    );
});
it('帧数、单事件大小和取消都有边界，超限后不误认结束', async () => {
  const p = await parse('data: ' + 'x'.repeat(1024 * 1024) + '\n\n' + delta('恢复') + end);
  expect(p.blocks[0]?.warning).toContain('1 MiB');
  expect(p.blocks[1]?.text).toBe('恢复');
  expect(await scanFrames(':\n\n'.repeat(20_001), signal(), () => {})).toEqual({ count: 20_000, limited: true });
  const controller = new AbortController(),
    parsing = responseProjection(delta('a').repeat(5000), 'text/event-stream', controller.signal);
  controller.abort();
  await expect(parsing).rejects.toThrow();
});
it('JSON、compact、计量不伪造事件；错误与空响应可辨', async () => {
  for (const value of [
    { input_tokens: 42 },
    { object: 'response.compaction', output: [{ type: 'compaction', encrypted_content: 'secret' }] },
    { error: { message: 'bad' } },
  ]) {
    const p = await responseProjection(JSON.stringify(value), 'application/json', signal());
    expect(p.stream).toBe(false);
    expect(p.frames).toBe(0);
  }
  expect((await responseProjection('', 'application/json', signal())).notices.join()).toContain('零字节');
});

it('推理摘要及拒绝内容按类别校准，不能混入输出文本', async () => {
  const p = await parse(
    event({ type: 'response.reasoning_summary_text.delta', output_index: 0, summary_index: 0, delta: '摘要' }) +
      event({
        type: 'response.reasoning_summary_part.done',
        output_index: 0,
        summary_index: 0,
        part: { text: '摘要' },
      }) +
      event({ type: 'response.refusal.delta', output_index: 1, content_index: 0, delta: '拒绝' }) +
      event({
        type: 'response.content_part.done',
        output_index: 1,
        content_index: 0,
        part: { type: 'refusal', refusal: '拒绝' },
      }) +
      event({
        type: 'response.completed',
        response: {
          id: 'r',
          status: 'completed',
          output: [
            { type: 'reasoning', summary: [{ text: '摘要' }] },
            { type: 'message', content: [{ type: 'refusal', refusal: '拒绝' }] },
          ],
        },
      }),
  );
  expect(p.blocks.filter((b) => b.key).map((b) => b.text)).toEqual(['摘要', '拒绝']);
  expect(p.blocks.some((b) => b.warning)).toBe(false);
});

it('跨响应身份和 choice 终态冲突不覆盖之前的内容', async () => {
  const p = await parse(
    event({ id: 'a', choices: [{ index: 0, delta: { content: '旧内容' }, finish_reason: 'stop' }] }) +
      event({ id: 'b', choices: [{ index: 0, delta: { content: '别的响应' } }] }) +
      event({ id: 'a', choices: [{ index: 0, delta: {}, finish_reason: 'length' }] }),
  );
  expect(p.blocks.filter((b) => b.warning).map((b) => b.title)).toContain('响应身份冲突');
  expect(p.blocks.filter((b) => b.warning).map((b) => b.title)).toContain('Choice 终态冲突');
  expect(p.blocks.filter((b) => b.key).map((b) => b.text)).toEqual(['旧内容']);
});

it('只有 item ID 的增量可由完成项建立别名，但不合并冲突 ID', async () => {
  const p = await parse(
    event({ type: 'response.output_text.delta', item_id: 'm', content_index: 0, delta: '内容' }) +
      event({
        type: 'response.completed',
        response: {
          id: 'r',
          status: 'completed',
          output: [{ id: 'm', type: 'message', content: [{ type: 'output_text', text: '内容' }] }],
        },
      }),
  );
  expect(p.blocks.filter((b) => b.key).map((b) => b.text)).toEqual(['内容']);
  const bad = await parse(
    event({ type: 'response.output_text.delta', output_index: 0, item_id: 'a', content_index: 0, delta: 'A' }) +
      event({ type: 'response.output_text.delta', output_index: 0, item_id: 'b', content_index: 0, delta: 'B' }),
  );
  expect(bad.blocks.filter((b) => b.key).map((b) => b.text)).toEqual(['A']);
  expect(bad.blocks.some((b) => b.warning)).toBe(true);
});
