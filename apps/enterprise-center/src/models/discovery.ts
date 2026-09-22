import { request as httpRequest } from 'node:http';
import { request as httpsRequest } from 'node:https';
import type { CatalogModel, ModelParameters, ProviderConnection, Support, TokenLimit } from '../contracts/models.js';
import { efforts } from '../contracts/models.js';
import { IdentityError } from '../identity/errors.js';
import { emptyParameters, enumeration, text } from './parameters.js';

const failure = () => new IdentityError(502, 'MODEL_DISCOVERY_FAILED');

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw failure();
  return value as Record<string, unknown>;
}

function limit(value: unknown): TokenLimit {
  if (value === null || value === undefined) return { state: 'unknown' };
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0
    ? { state: 'known', value }
    : { state: 'invalid' };
}

function boolean(value: unknown): Support {
  if (value === null || value === undefined) return 'unknown';
  if (typeof value !== 'boolean') throw failure();
  return value ? 'supported' : 'unsupported';
}

function feature(value: unknown, name: string): Support {
  if (value === null || value === undefined) return 'unknown';
  if (!Array.isArray(value) || value.some((v) => typeof v !== 'string')) throw failure();
  return value.includes(name) ? 'supported' : 'unsupported';
}

function identity(raw: Record<string, unknown>, field: string): CatalogModel {
  const model_id = text(raw[field], 512);
  if (model_id.trim() !== model_id) throw failure();
  return { model_id, display_name: null, metadata: emptyParameters() };
}

function display(value: unknown): string | null {
  if (value === undefined || value === null) return null;
  const name = text(value, 256);
  if (name.trim() !== name) throw failure();
  return name;
}

/** 只识别已核查发现字段；缺失能力不推断为支持，全部分页成功前不发布结果。 */
export function parsePage(
  value: unknown,
  format: ProviderConnection['discovery_format'],
  page: number,
): { models: CatalogModel[]; total?: number } {
  const document = object(value);
  if (format === 'dashscope_native') {
    if (document.success !== true) throw failure();
    const output = object(document.output),
      total = output.total,
      size = output.page_size;
    if (
      typeof total !== 'number' ||
      !Number.isSafeInteger(total) ||
      total < 0 ||
      total > 10000 ||
      output.page_no !== page ||
      typeof size !== 'number' ||
      !Number.isInteger(size) ||
      size < 1 ||
      size > 100 ||
      !Array.isArray(output.models) ||
      output.models.length > size ||
      (!output.models.length && total !== 0)
    )
      throw failure();
    return {
      total,
      models: output.models.map((value) => {
        const raw = object(value),
          model = identity(raw, 'model');
        model.display_name = display(raw.name);
        const info = raw.model_info == null ? {} : object(raw.model_info),
          inference = raw.inference_metadata == null ? {} : object(raw.inference_metadata);
        const p = model.metadata;
        p.context_window_tokens = limit(info.context_window);
        for (const key of [
          'max_input_tokens',
          'max_output_tokens',
          'reasoning_max_input_tokens',
          'reasoning_max_output_tokens',
        ] as const) {
          p[key] = limit(info[key]);
          if (
            p[key].value !== undefined &&
            p.context_window_tokens.value !== undefined &&
            p[key].value! > p.context_window_tokens.value
          )
            p[key] = { state: 'invalid' };
        }
        p.image_input = feature(inference.request_modality, 'Image');
        p.tool_calls = feature(raw.features, 'function-calling');
        p.reasoning = feature(raw.capabilities, 'Reasoning');
        return model;
      }),
    };
  }
  if (
    document.object !== 'list' ||
    (document.has_more !== undefined && document.has_more !== false) ||
    !Array.isArray(document.data) ||
    document.data.length > 10000
  )
    throw failure();
  return {
    models: document.data.map((value) => {
      const raw = object(value);
      if (raw.object !== 'model') throw failure();
      const model = identity(raw, 'id');
      if (format === 'vllm') model.metadata.context_window_tokens = limit(raw.max_model_len);
      if (format === 'moonshot') {
        model.display_name = display(raw.display_name);
        model.metadata = moonshot(raw);
      }
      return model;
    }),
  };
}

function moonshot(raw: Record<string, unknown>): ModelParameters {
  const p = emptyParameters();
  p.context_window_tokens = limit(raw.context_length);
  p.image_input = boolean(raw.supports_image_in);
  p.reasoning = boolean(raw.supports_reasoning);
  if (raw.supports_thinking_type != null) {
    if (raw.supports_thinking_type !== 'only' || p.reasoning === 'unsupported') throw failure();
    p.reasoning_mode = 'always';
  }
  if (raw.think_efforts == null) return p;
  const map = object(raw.think_efforts);
  if (map.support === true) {
    if (
      !Array.isArray(map.valid_efforts) ||
      !map.valid_efforts.length ||
      map.valid_efforts.length > 5 ||
      new Set(map.valid_efforts).size !== map.valid_efforts.length ||
      p.reasoning === 'unsupported'
    )
      throw failure();
    p.reasoning_efforts = {};
    for (const value of map.valid_efforts) {
      const wire = enumeration(value, ['low', 'medium', 'high', 'xhigh', 'max']);
      p.reasoning_efforts[wire] = wire;
    }
    p.default_reasoning_effort = enumeration(map.default_effort, efforts);
    if (!Object.hasOwn(p.reasoning_efforts, p.default_reasoning_effort)) throw failure();
  } else if (map.support === false) {
    if (
      (map.valid_efforts != null && (!Array.isArray(map.valid_efforts) || map.valid_efforts.length)) ||
      map.default_effort != null
    )
      throw failure();
    p.reasoning_efforts = {};
  } else throw failure();
  return p;
}

function readPage(url: URL, key: string, signal: AbortSignal): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const headers: Record<string, string> = { Accept: 'application/json', 'Accept-Encoding': 'identity' };
    if (key) headers.Authorization = `Bearer ${key}`;
    const request = (url.protocol === 'https:' ? httpsRequest : httpRequest)(
      url,
      { method: 'GET', headers, signal },
      (response) => {
        if (
          !response.statusCode ||
          response.statusCode < 200 ||
          response.statusCode >= 300 ||
          (response.headers['content-encoding'] && response.headers['content-encoding'] !== 'identity')
        ) {
          response.destroy();
          reject(failure());
          return;
        }
        const chunks: Buffer[] = [];
        let length = 0;
        response.on('data', (chunk: Buffer) => {
          length += chunk.length;
          if (length > 1024 * 1024) {
            response.destroy();
            reject(failure());
          } else chunks.push(chunk);
        });
        response.once('end', () => resolve(Buffer.concat(chunks, length)));
        response.once('error', () => reject(failure()));
        response.once('aborted', () => reject(failure()));
      },
    );
    const timer = setTimeout(() => request.destroy(failure()), 5000);
    request.once('socket', (socket) => {
      if (!socket.connecting) clearTimeout(timer);
      else socket.once(url.protocol === 'https:' ? 'secureConnect' : 'connect', () => clearTimeout(timer));
    });
    request.once('close', () => clearTimeout(timer));
    request.once('error', () => reject(failure()));
    request.end();
  });
}

export async function discover(
  connection: ProviderConnection,
  key: string,
  shutdown: AbortSignal,
): Promise<CatalogModel[]> {
  const signal = AbortSignal.any([shutdown, AbortSignal.timeout(20000)]),
    url = new URL(connection.endpoint);
  if (connection.models_path) url.pathname = connection.models_path;
  else if (connection.discovery_format === 'dashscope_native') url.pathname = '/api/v1/models';
  else url.pathname = `${url.pathname.replace(/\/$/, '')}/models`;
  const models: CatalogModel[] = [],
    ids = new Set<string>();
  let bytes = 0,
    total: number | undefined;
  try {
    for (let page = 1; page <= 100; page++) {
      if (connection.discovery_format === 'dashscope_native') {
        url.searchParams.set('page_no', String(page));
        url.searchParams.set('page_size', '100');
      }
      const buffer = await readPage(url, key, signal);
      bytes += buffer.length;
      if (bytes > 8 * 1024 * 1024) throw failure();
      const result = parsePage(
        JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(buffer)),
        connection.discovery_format,
        page,
      );
      if (total !== undefined && result.total !== total) throw failure();
      total = result.total;
      for (const model of result.models) {
        if (ids.has(model.model_id)) throw failure();
        ids.add(model.model_id);
        models.push(model);
      }
      if (models.length > 10000 || (total !== undefined && models.length > total)) throw failure();
      if (total === undefined || models.length === total) {
        if (Buffer.byteLength(JSON.stringify(models)) > 8 * 1024 * 1024) throw failure();
        return models;
      }
    }
  } catch {
    throw failure();
  }
  throw failure();
}
