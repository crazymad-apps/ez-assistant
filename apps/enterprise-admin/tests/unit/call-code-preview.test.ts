import { expect, it } from 'vitest';
import { codeTokens, prepareCodePreview } from '../../src/features/calls/CodePreview/preview';

const preview = (text: string) => prepareCodePreview(text, new AbortController().signal);

it('格式化只改变空白，大整数、重复键、指数和转义仍是原字符', async () => {
  const raw = '{"n":900719925474099312345,"n":1e+30,"s":"\\u4f60\\n</script>","zero":-0}';
  const result = await preview(raw);
  expect(result.formatted).toBe(
    '{\n  "n": 900719925474099312345,\n  "n": 1e+30,\n  "s": "\\u4f60\\n</script>",\n  "zero": -0\n}',
  );
  const formatted = result.formatted!;
  const tokens = codeTokens(formatted);
  expect(tokens.map((t) => formatted.slice(t.start, t.end)).join('')).toBe(formatted);
  expect(tokens.filter((t) => t.tone === 'key')).toHaveLength(4);
});

it('单条 SSE 的多行 JSON 可读，多个事件、完成标记和尾部残片保留原文', async () => {
  for (const eol of ['\n', '\r\n', '\r']) {
    const event =
      '\uFEFFevent: codex.rate_limits' + eol + 'id: 12' + eol + 'data: {"allowed":' + eol + 'data: true}' + eol + eol;
    expect(await preview(event)).toMatchObject({ event: 'codex.rate_limits', formatted: '{\n  "allowed": true\n}' });
    expect((await preview(event + 'data: {}' + eol + eol)).formatted).toBeUndefined();
  }
  expect((await preview('data: [DONE]\n\n')).formatted).toBeUndefined();
  expect(await preview('event: error\ndata: {"error":true}')).toMatchObject({
    notice: expect.stringContaining('事件不完整'),
  });
});

it('坏 JSON、不完整内容和 JSONC 不被容错解析器修复为完整数据', async () => {
  for (const text of ['{"x":', '{"x":1,}', '{/* comment */"x":1}', '{"x": NaN}', '{"x":01}', '{"x":1} trailing']) {
    const result = await preview(text);
    expect(result.formatted).toBeUndefined();
    expect(result.notice).toContain('异常');
  }
  expect((await preview('data: {"x":\n\n')).notice).toContain('异常');
});

it('尺寸、深度和高亮节点有界，长文本完整保留且取消不提交', async () => {
  expect((await preview('['.repeat(65) + '0' + ']'.repeat(65))).formatted).toBeUndefined();
  expect((await preview(' '.repeat(256 * 1024) + '{}')).formatted).toBeUndefined();
  const text = '[0,'.repeat(10_000) + 'tail';
  const tokens = codeTokens(text);
  expect(tokens.length).toBeLessThanOrEqual(4001);
  expect(tokens.map((t) => text.slice(t.start, t.end)).join('')).toBe(text);
  const controller = new AbortController();
  controller.abort();
  await expect(prepareCodePreview('{}', controller.signal)).rejects.toThrow();
});
