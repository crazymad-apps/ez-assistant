import { applyEdits, createScanner, format, visit } from 'jsonc-parser';
import { scanFrames } from '../BodyReader/frames';

const formatLimit = 256 * 1024;
const outputLimit = 512 * 1024;
export type CodePreviewValue = { formatted?: string; event?: string; notice?: string };
export type CodeToken = { start: number; end: number; tone?: 'key' | 'string' | 'number' | 'literal' };

/** 只编辑空白，不经对象反序列化重建，保留大整数、重复键、字段顺序和转义。 */
function formatJson(text: string): CodePreviewValue {
  const scanner = createScanner(text, true);
  let depth = 0;
  let count = 0;
  for (scanner.scan(); scanner.getTokenLength() > 0; scanner.scan()) {
    const lexeme = text.slice(scanner.getTokenOffset(), scanner.getTokenOffset() + scanner.getTokenLength());
    if (lexeme === '{' || lexeme === '[') depth++;
    if (lexeme === '}' || lexeme === ']') depth--;
    if (depth > 64 || ++count > 20_000) return { notice: '内容层级或字段较多，按原文分段展示。' };
  }
  let invalid = false;
  visit(
    text,
    {
      onError: () => {
        invalid = true;
      },
    },
    { disallowComments: true, allowTrailingComma: false },
  );
  if (invalid) {
    return /^\s*[{[]/.test(text) ? { notice: 'JSON 不完整或格式异常，保留原文，不自动修复。' } : {};
  }
  const edits = format(text, undefined, { tabSize: 2, insertSpaces: true, eol: '\n' });
  const size = edits.reduce((length, edit) => length + edit.content.length - edit.length, text.length);
  if (size > outputLimit) return { notice: '格式化后内容较长，按原文分段展示。' };
  return { formatted: applyEdits(text, edits) };
}

/** 来源片段复用现有 SSE 分帧器；多个事件或尾部残片不伪造成一个完整 JSON。 */
export async function prepareCodePreview(text: string, signal: AbortSignal): Promise<CodePreviewValue> {
  signal.throwIfAborted();
  if (text.length > formatLimit) return { notice: '内容较长，按原文分段展示。' };
  if (!text.trim()) return {};
  if (/^\uFEFF?(?:event:|data:|id:|retry:|:)/.test(text)) {
    const frames: { data: string | null; event: string; problem?: string }[] = [];
    await scanFrames(text, signal, (source, data, problem) => frames.push({ data, event: source.type, problem }));
    if (frames.length !== 1) return {};
    const frame = frames[0]!;
    if (frame.problem) return { notice: '事件不完整，保留原文，不自动修复。' };
    if (frame.data == null || frame.data === '[DONE]') return {};
    const preview = formatJson(frame.data);
    return { ...preview, event: preview.formatted === undefined ? undefined : frame.event };
  }
  return formatJson(text);
}

/** 按扫描位置截取原字符，React 文本节点负责转义；限制高亮节点数，余下内容仍可读。 */
export function codeTokens(text: string): CodeToken[] {
  const scanner = createScanner(text);
  const tokens: CodeToken[] = [];
  let previous: CodeToken | undefined;
  for (scanner.scan(); scanner.getTokenLength() > 0; scanner.scan()) {
    const start = scanner.getTokenOffset();
    if (tokens.length >= 4000 || start >= 65536) {
      tokens.push({ start, end: text.length });
      break;
    }
    const token: CodeToken = { start, end: start + scanner.getTokenLength() };
    const lexeme = text.slice(token.start, token.end);
    if (lexeme.startsWith('"')) token.tone = 'string';
    if (/^-?\d/.test(lexeme)) token.tone = 'number';
    if (['true', 'false', 'null'].includes(lexeme)) token.tone = 'literal';
    if (lexeme === ':' && previous?.tone === 'string') previous.tone = 'key';
    if (lexeme.trim()) previous = token;
    tokens.push(token);
  }
  return tokens;
}
