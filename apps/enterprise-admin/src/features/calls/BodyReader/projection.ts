/** 阅读投影只属于当前详情页；位置是原字符串的 UTF-16 区间，不是网络 chunk 或持久事件 ID。 */
export type Source = { start: number; end: number; number: number; type: string };
export type Block = {
  title: string;
  text?: string;
  key?: string;
  status?: string;
  detail?: string;
  warning?: string;
  conflict?: string;
  deltaCount?: number;
  continuation?: boolean;
  sources: Source[];
};
export type Projection = { blocks: Block[]; notices: string[]; metadata?: string; stream: boolean; frames: number };
export const object = (value: unknown): Record<string, unknown> =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
export const array = (value: unknown): unknown[] => (Array.isArray(value) ? value : []);
export const string = (value: unknown): string => (typeof value === 'string' ? value : '');
export const index = (value: unknown): string =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? String(value) : '';
export function structured(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2) ?? '';
  } catch {
    return '字段层级超过结构化展示能力，请查看原文。';
  }
}
export const emptyProjection = (): Projection => ({ blocks: [], notices: [], stream: false, frames: 0 });

/** 让出主线程后再检查取消，收起/换调用时旧投影不得提交。 */
export async function yieldReading(signal: AbortSignal) {
  signal.throwIfAborted();
  await new Promise<void>((done) => setTimeout(done, 0));
  signal.throwIfAborted();
}

export function parseError(error: unknown): string {
  // 只采纳解析器实际报告的位置，不推算修复后的位置，也不把错误正文写入日志。
  const message = error instanceof Error ? error.message : '';
  const position = /position (\d+)/.exec(message);
  return position ? `JSON 无法完整解析，字符位置 ${position[1]}（从 0 开始）` : 'JSON 无法完整解析，位置未确定';
}
