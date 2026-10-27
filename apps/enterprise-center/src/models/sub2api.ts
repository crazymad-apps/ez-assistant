import type { CatalogModel } from '../contracts/models.js';
import { efforts } from '../contracts/models.js';
import { IdentityError } from '../identity/errors.js';
import { emptyParameters, enumeration, text } from './parameters.js';

const invalid = () => new IdentityError(502, 'MODEL_DISCOVERY_FAILED');

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalid();
  return value as Record<string, unknown>;
}

/** 只投影 Sub2API 的能力声明；客户端提示词、截断策略和压缩阈值不是模型能力。 */
export function parseSub2api(value: unknown): CatalogModel[] {
  const document = object(value);
  if (!Array.isArray(document.models) || document.models.length > 10000) throw invalid();
  return document.models.map((value) => {
    const raw = object(value);
    const model_id = text(raw.slug, 512);
    if (model_id.trim() !== model_id) throw invalid();
    let display_name: string | null = null;
    if (raw.display_name != null) {
      display_name = text(raw.display_name, 256);
      if (display_name.trim() !== display_name) throw invalid();
    }
    const metadata = emptyParameters();
    const context = raw.context_window ?? raw.max_context_window;
    if (context != null) {
      metadata.context_window_tokens = typeof context === 'number' && Number.isSafeInteger(context) && context > 0
        ? { state: 'known', value: context } : { state: 'invalid' };
    }
    if (raw.input_modalities != null) {
      if (!Array.isArray(raw.input_modalities) || raw.input_modalities.some((value) => typeof value !== 'string'))
        throw invalid();
      metadata.image_input = raw.input_modalities.includes('image') ? 'supported' : 'unsupported';
    }
    if (raw.supports_parallel_tool_calls != null) {
      if (typeof raw.supports_parallel_tool_calls !== 'boolean') throw invalid();
      // 否定并行并不否定串行工具调用。
      if (raw.supports_parallel_tool_calls) metadata.tool_calls = 'supported';
    }
    if (raw.supported_reasoning_levels != null) {
      if (!Array.isArray(raw.supported_reasoning_levels) || raw.supported_reasoning_levels.length > 6) throw invalid();
      const seen = new Set<string>();
      const mapping: NonNullable<typeof metadata.reasoning_efforts> = {};
      for (const level of raw.supported_reasoning_levels) {
        const effort = enumeration(object(level).effort, [...efforts, 'none']);
        if (seen.has(effort)) throw invalid();
        seen.add(effort);
        if (effort !== 'none') mapping[effort] = effort;
      }
      const selected = raw.default_reasoning_level;
      if (selected != null && (typeof selected !== 'string' || !seen.has(selected))) throw invalid();
      if (Object.keys(mapping).length) {
        metadata.reasoning = 'supported';
        // none 代表沿用模型默认行为，不能伪装成关闭思考或标准强度档位。
        if (selected != null && selected !== 'none') {
          metadata.default_reasoning_effort = enumeration(selected, efforts);
          metadata.reasoning_efforts = mapping;
        }
      } else metadata.reasoning_efforts = {};
    } else if (raw.default_reasoning_level != null) throw invalid();
    return { model_id, display_name, metadata };
  });
}
