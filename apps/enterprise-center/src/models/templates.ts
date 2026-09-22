import { open } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import type {
  ModelParameters,
  ProviderConnection,
  ProviderType,
  Protocol,
  TemplateStatus,
} from '../contracts/models.js';
import { providerTypes, protocols } from '../contracts/models.js';
import { fields, invalid } from '../validation.js';
import { emptyParameters, enumeration, parseParameters, resolveProtocol, text, validParameters } from './parameters.js';

type Template = {
  provider_type: ProviderType;
  model_id: string;
  parameters: ModelParameters;
  protocol_parameters: Partial<Record<Protocol, ModelParameters>>;
  document: string;
  checked_on: string;
};

export function parseTemplates(value: unknown): Template[] {
  if (!Array.isArray(value) || value.length > 10000) invalid();
  const seen = new Set<string>();
  return value.map((raw) => {
    const item = fields(
      raw,
      ['provider_type', 'model_id', 'parameters', 'document', 'checked_on'],
      ['protocol_parameters'],
    );
    const provider_type = enumeration(item.provider_type, providerTypes),
      model_id = text(item.model_id, 512);
    const key = JSON.stringify([provider_type, model_id]);
    if (seen.has(key) || model_id.trim() !== model_id) invalid();
    seen.add(key);
    const document = text(item.document, 2048),
      checked_on = text(item.checked_on, 10);
    let url: URL;
    try {
      url = new URL(document);
    } catch {
      return invalid();
    }
    if (
      url.protocol !== 'https:' ||
      url.username ||
      url.password ||
      !/^\d{4}-\d{2}-\d{2}$/.test(checked_on) ||
      !Number.isFinite(Date.parse(checked_on)) ||
      new Date(checked_on).toISOString().slice(0, 10) !== checked_on
    )
      invalid();
    const parameters = parseParameters(item.parameters, true);
    if (!validParameters(parameters, false)) invalid();
    const overrides = fields(item.protocol_parameters ?? {}, [], protocols);
    const protocol_parameters: Template['protocol_parameters'] = {};
    for (const protocol of protocols)
      if (Object.hasOwn(overrides, protocol)) {
        const params = parseParameters(overrides[protocol], true, parameters);
        if (
          !validParameters(params, false) ||
          (protocol === 'open_ai_chat_completions' && params.tool_image_projection === 'native_tool_result')
        )
          invalid();
        protocol_parameters[protocol] = params;
      }
    return { provider_type, model_id, parameters, protocol_parameters, document, checked_on };
  });
}

/** 文件是模板权威；只在完整读取/校验及管理审计提交后发布不可变快照。 */
export class ModelTemplates {
  private entries: readonly Template[] | undefined;
  private loadedAt: number | null = null;
  private failure: string | null = null;

  constructor(private readonly path = fileURLToPath(new URL('../resources/model-templates.json', import.meta.url))) {}

  status(): TemplateStatus {
    return {
      available: this.entries !== undefined,
      count: this.entries?.length ?? 0,
      loaded_at_ms: this.loadedAt,
      error: this.failure,
    };
  }

  async read(): Promise<Template[]> {
    const handle = await open(this.path, 'r');
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.size > 8 * 1024 * 1024) invalid();
      // 有界读取防止检查后文件增长；只读至上限加一，不跟随无限增长文件。
      const buffer = Buffer.alloc(8 * 1024 * 1024 + 1);
      let length = 0;
      while (length < buffer.length) {
        const read = await handle.read(buffer, length, buffer.length - length, null);
        if (!read.bytesRead) break;
        length += read.bytesRead;
      }
      if (length > 8 * 1024 * 1024) invalid();
      return parseTemplates(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(buffer.subarray(0, length))));
    } finally {
      await handle.close();
    }
  }

  publish(entries: readonly Template[]): void {
    this.entries = entries;
    this.loadedAt = Date.now();
    this.failure = null;
  }

  failed(): void {
    this.failure = '模板读取或校验失败，请检查文件；已有有效模板继续使用。';
  }

  async initialize(): Promise<void> {
    try {
      this.publish(await this.read());
    } catch {
      this.failed();
    }
  }

  lookup(connection: ProviderConnection, model: string) {
    const entry = this.entries?.find(
      (item) => item.provider_type === connection.provider_type && item.model_id === model,
    );
    if (!entry) return { parameters: emptyParameters(), document: null, checked_on: null };
    return {
      parameters: entry.protocol_parameters[resolveProtocol(connection)] ?? entry.parameters,
      document: entry.document,
      checked_on: entry.checked_on,
    };
  }
}
