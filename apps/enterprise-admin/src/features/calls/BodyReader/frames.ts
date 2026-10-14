import { yieldReading, type Source } from './projection';

const frameLimit = 1024 * 1024;
export const frameCountLimit = 20_000;

/** 单次扫描原文；CR/LF/CRLF 均作为行结束。坏帧与超限帧仅在真正空行处分隔。 */
export async function scanFrames(
  text: string,
  signal: AbortSignal,
  consume: (source: Source, data: string | null, problem?: string) => void,
) {
  let start = 0,
    lineStart = text.charCodeAt(0) === 0xfeff ? 1 : 0,
    count = 0,
    yielded = 0;
  let data: string[] = [],
    event = '',
    meaningful = false,
    oversized = false;
  const finish = (end: number, tail = false) => {
    const rawLength = end - start;
    if (rawLength <= 0) return;
    const source = { start, end, number: ++count, type: event || 'message' };
    const tooBig = oversized || new TextEncoder().encode(text.slice(start, end)).byteLength > frameLimit;
    consume(
      source,
      tooBig ? null : meaningful ? data.join('\n') : null,
      tooBig ? '展示解析已到单事件 1 MiB 上限' : tail ? '尾部事件未完整结束' : undefined,
    );
    start = end;
    data = [];
    event = '';
    meaningful = false;
    oversized = false;
  };
  for (let i = lineStart; i < text.length; i++) {
    if (i - yielded >= 32768) {
      await yieldReading(signal);
      yielded = i;
    }
    const char = text.charCodeAt(i);
    if (char !== 10 && char !== 13) continue;
    const end = char === 13 && text.charCodeAt(i + 1) === 10 ? i + 2 : i + 1;
    if (i === lineStart) {
      finish(end);
      if (count >= frameCountLimit) return { count, limited: end < text.length };
    } else {
      if (end - start > frameLimit) oversized = true;
      if (!oversized) {
        const line = text.slice(lineStart, i);
        const colon = line.indexOf(':');
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? '' : line.slice(colon + 1);
        if (value.startsWith(' ')) value = value.slice(1);
        if (field === 'data') {
          meaningful = true;
          data.push(value);
        }
        if (field === 'event') event = value;
      }
    }
    lineStart = end;
    i = end - 1;
  }
  if (start < text.length) finish(text.length, true);
  signal.throwIfAborted();
  return { count, limited: false };
}
