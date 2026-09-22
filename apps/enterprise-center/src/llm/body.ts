import type { FastifyInstance, FastifyRequest } from 'fastify';
import { ControlError } from './errors.js';

export const requestLimit = 32 * 1024 * 1024;
const totalLimit = 128 * 1024 * 1024;

/** 只提取准入字段；原始 Buffer 始终用于出站，避免重编码改变数字和未知字段。 */
export function requestModel(body: Buffer): { model: string; stream: boolean } {
  let text: string;
  let value: unknown;
  try {
    text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(body);
    value = JSON.parse(text);
  } catch {
    throw new ControlError('llm_request_invalid');
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new ControlError('llm_request_invalid');
  const object = value as Record<string, unknown>;
  if (
    typeof object.model !== 'string' ||
    !object.model ||
    object.model.length > 1024 ||
    /[\p{Cc}]/u.test(object.model) ||
    (object.stream !== undefined && typeof object.stream !== 'boolean')
  )
    throw new ControlError('llm_request_invalid');
  // JSON.parse 已校验语法；线性扫描层级与字符串，拒绝转义拼写的重复准入键，避免 last-wins 歧义。
  let depth = 0;
  const keys = new Set<string>();
  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (char === '{' || char === '[') depth++;
    else if (char === '}' || char === ']') depth--;
    else if (char === '"') {
      const start = i++;
      for (; i < text.length; i++) {
        if (text[i] === '\\') i++;
        else if (text[i] === '"') break;
      }
      if (depth !== 1) continue;
      let next = i + 1;
      while (/\s/.test(text[next] ?? '') && next < text.length) next++;
      if (text[next] !== ':') continue;
      const key: string = JSON.parse(text.slice(start, i + 1));
      if (key !== 'model' && key !== 'stream') continue;
      if (keys.has(key)) throw new ControlError('llm_request_invalid');
      keys.add(key);
    }
  }
  return { model: object.model, stream: object.stream === true };
}

/** 每个 scope 一个预算 owner；逐 chunk 预留，所有失败/断连/正常回复共用幂等释放。 */
export class RequestBodies {
  private bytes = 0;
  private readonly reservations = new Map<FastifyRequest, number>();

  release(request: FastifyRequest): void {
    this.bytes -= this.reservations.get(request) ?? 0;
    this.reservations.delete(request);
    request.body = undefined;
  }

  install(scope: FastifyInstance): void {
    scope.removeAllContentTypeParsers();
    scope.addContentTypeParser('application/json', (request, payload, done) => {
      let chunks: Buffer[] = [];
      let size = 0;
      let finished = false;

      const complete = (error?: Error) => {
        if (finished) return;
        finished = true;
        payload.removeListener('data', data);
        payload.removeListener('end', end);
        payload.removeListener('error', failed);
        request.raw.removeListener('aborted', aborted);
        if (error) {
          chunks = [];
          payload.pause();
          this.release(request);
          done(error);
        } else {
          const body = Buffer.concat(chunks, size);
          chunks = [];
          done(null, body);
        }
      };

      const data = (chunk: Buffer) => {
        if (size + chunk.length > requestLimit) return complete(new ControlError('llm_request_too_large'));
        if (this.bytes + chunk.length > totalLimit) return complete(new ControlError('llm_proxy_unavailable'));
        size += chunk.length;
        this.bytes += chunk.length;
        this.reservations.set(request, size);
        chunks.push(chunk);
      };

      const end = () => complete();

      const failed = () => complete(new ControlError('llm_request_invalid'));

      const aborted = () => failed();

      payload.on('data', data);
      payload.once('end', end);
      payload.once('error', failed);
      request.raw.once('aborted', aborted);
    });
    scope.addHook('onResponse', async (request) => this.release(request));
    scope.addHook('onRequestAbort', async (request) => this.release(request));
  }
}
