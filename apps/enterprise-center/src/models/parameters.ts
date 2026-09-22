import type { ModelParameters, Protocol, ProviderConnection, Support, TokenLimit } from '../contracts/models.js';
import { supports, efforts, providerTypes, preferences } from '../contracts/models.js';
import { fields, invalid } from '../validation.js';

const limits = [
  'context_window_tokens',
  'max_input_tokens',
  'max_output_tokens',
  'reasoning_max_input_tokens',
  'reasoning_max_output_tokens',
] as const;
const features = ['streaming', 'image_input', 'tool_calls', 'reasoning'] as const;

export function emptyParameters(): ModelParameters {
  return {
    context_window_tokens: { state: 'unknown' },
    max_input_tokens: { state: 'unknown' },
    max_output_tokens: { state: 'unknown' },
    reasoning_max_input_tokens: { state: 'unknown' },
    reasoning_max_output_tokens: { state: 'unknown' },
    streaming: 'unknown',
    image_input: 'unknown',
    tool_calls: 'unknown',
    reasoning: 'unknown',
    reasoning_mode: 'unknown',
    tool_choice: { auto: 'unknown', none: 'unknown', required: 'unknown', named: 'unknown' },
    tool_image_projection: 'unknown',
    reasoning_efforts: null,
    default_reasoning_effort: null,
  };
}

export function text(value: unknown, max: number, empty = false): string {
  if (
    typeof value !== 'string' ||
    (!empty && !value.trim()) ||
    Buffer.byteLength(value) > max ||
    /[\p{Cc}]/u.test(value)
  )
    invalid();
  return value;
}

export function enumeration<T extends string>(value: unknown, values: readonly T[]): T {
  if (typeof value !== 'string' || !values.includes(value as T)) invalid();
  return value as T;
}

export function uuid(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value))
    invalid();
  return value;
}

export function parseParameters(value: unknown, partial = false, base = emptyParameters()): ModelParameters {
  const input = fields(value, partial ? [] : Object.keys(base), partial ? Object.keys(base) : []);
  const result = structuredClone(base);
  for (const key of limits)
    if (Object.hasOwn(input, key)) {
      const limit = fields(input[key], ['state'], ['value']);
      const state = enumeration(limit.state, ['unknown', 'known', 'invalid']);
      if (state === 'known') {
        if (typeof limit.value !== 'number' || !Number.isSafeInteger(limit.value) || limit.value < 1) invalid();
        result[key] = { state, value: limit.value };
      } else {
        if (Object.hasOwn(limit, 'value')) invalid();
        result[key] = { state };
      }
    }
  for (const key of features) if (Object.hasOwn(input, key)) result[key] = enumeration(input[key], supports);
  if (Object.hasOwn(input, 'tool_choice')) {
    const choice = fields(
      input.tool_choice,
      partial ? [] : Object.keys(base.tool_choice),
      partial ? Object.keys(base.tool_choice) : [],
    );
    for (const key of ['auto', 'none', 'required', 'named'] as const)
      if (Object.hasOwn(choice, key)) result.tool_choice[key] = enumeration(choice[key], supports);
  }
  if (Object.hasOwn(input, 'tool_image_projection'))
    result.tool_image_projection = enumeration(input.tool_image_projection, [
      'unknown',
      'unsupported',
      'native_tool_result',
      'follow_up_user_message',
    ]);
  if (Object.hasOwn(input, 'reasoning_mode'))
    result.reasoning_mode = enumeration(input.reasoning_mode, ['unknown', 'unsupported', 'optional', 'always']);
  if (Object.hasOwn(input, 'reasoning_efforts')) {
    result.reasoning_efforts = null;
    if (input.reasoning_efforts !== null) {
      const map = fields(input.reasoning_efforts, [], efforts);
      result.reasoning_efforts = {};
      for (const key of efforts) if (Object.hasOwn(map, key)) result.reasoning_efforts[key] = text(map[key], 128);
    }
  }
  if (Object.hasOwn(input, 'default_reasoning_effort'))
    result.default_reasoning_effort =
      input.default_reasoning_effort === null ? null : enumeration(input.default_reasoning_effort, efforts);
  return result;
}

/** 与 Rust 固定参数校验同义；模板允许未知必需值，但不能带矛盾配置。 */
export function validParameters(p: ModelParameters, complete: boolean): boolean {
  const context = p.context_window_tokens.value;
  if (complete && (p.context_window_tokens.state !== 'known' || p.max_output_tokens.state !== 'known')) return false;
  for (const key of limits) {
    const limit = p[key];
    if (
      limit.state === 'invalid' ||
      (limit.value !== undefined &&
        ((context !== undefined && limit.value > context) ||
          (key.endsWith('output_tokens') && limit.value > 4294967295)))
    )
      return false;
  }
  const hasReasoning =
    ['optional', 'always'].includes(p.reasoning_mode) || Object.keys(p.reasoning_efforts ?? {}).length > 0;
  if (
    (p.reasoning === 'unsupported' && hasReasoning) ||
    (p.reasoning_mode === 'unsupported' && (p.reasoning === 'supported' || hasReasoning))
  )
    return false;
  if (
    (p.reasoning === 'unsupported' || p.reasoning_mode === 'unsupported') &&
    (p.reasoning_max_input_tokens.state === 'known' || p.reasoning_max_output_tokens.state === 'known')
  )
    return false;
  const image = ['native_tool_result', 'follow_up_user_message'].includes(p.tool_image_projection);
  if (
    (p.tool_calls === 'unsupported' &&
      (image || [p.tool_choice.auto, p.tool_choice.required, p.tool_choice.named].includes('supported'))) ||
    (p.image_input === 'unsupported' && image)
  )
    return false;
  if (p.default_reasoning_effort !== null && !Object.hasOwn(p.reasoning_efforts ?? {}, p.default_reasoning_effort))
    return false;
  if (Object.keys(p.reasoning_efforts ?? {}).length > 0 && p.default_reasoning_effort === null) return false;
  return true;
}

export function resolveProtocol(connection: ProviderConnection): Protocol {
  const responses = ['openai', 'deepseek', 'dashscope_api', 'dashscope_plan', 'moonshot'].includes(
    connection.provider_type,
  );
  if (connection.protocol_preference === 'responses' && !responses) invalid();
  return connection.protocol_preference !== 'chat_completions' && responses
    ? 'open_ai_responses'
    : 'open_ai_chat_completions';
}

export function executable(connection: ProviderConnection, parameters: ModelParameters): boolean {
  return (
    validParameters(parameters, true) &&
    parameters.streaming === 'supported' &&
    !(
      resolveProtocol(connection) === 'open_ai_chat_completions' &&
      parameters.tool_image_projection === 'native_tool_result'
    )
  );
}

export function parseConnection(value: unknown): ProviderConnection {
  const input = fields(value, [
    'display_name',
    'provider_type',
    'endpoint',
    'protocol_preference',
    'models_path',
    'discovery_format',
  ]);
  const provider_type = enumeration(input.provider_type, providerTypes);
  const endpoint = text(input.endpoint, 8192);
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return invalid();
  }
  if (
    !['http:', 'https:'].includes(url.protocol) ||
    !url.hostname ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  )
    invalid();
  const models_path = text(input.models_path, 2048, true);
  if (
    models_path &&
    (!/^\/[a-zA-Z0-9/_.~-]*$/.test(models_path) ||
      models_path.startsWith('//') ||
      models_path.split('/').some((p) => p === '.' || p === '..'))
  )
    invalid();
  let discovery_format: ProviderConnection['discovery_format'] = 'openai';
  if (provider_type === 'dashscope_api') discovery_format = 'dashscope_native';
  else if (provider_type === 'vllm' || provider_type === 'moonshot') discovery_format = provider_type;
  if (input.discovery_format !== discovery_format) invalid();
  const connection = {
    display_name: text(input.display_name, 256),
    provider_type,
    endpoint,
    models_path,
    discovery_format,
    protocol_preference: enumeration(input.protocol_preference, preferences),
  };
  resolveProtocol(connection);
  return connection;
}

/** 按字段补未知；Invalid、明确不支持及空档位集合都是在线事实。 */
export function mergeParameters(online: ModelParameters, template: ModelParameters) {
  const parameters = structuredClone(online),
    sources: Record<string, string> = {};
  for (const key of limits) {
    const value: TokenLimit = parameters[key];
    if (value.state !== 'unknown') sources[key] = 'online';
    else {
      parameters[key] = structuredClone(template[key]);
      sources[key] = template[key].state === 'unknown' ? 'unconfigured' : 'template';
    }
  }

  function fill(
    key: 'streaming' | 'image_input' | 'tool_calls' | 'reasoning' | 'reasoning_mode' | 'tool_image_projection',
  ) {
    if (parameters[key] !== 'unknown') sources[key] = 'online';
    else {
      Object.assign(parameters, { [key]: template[key] });
      sources[key] = template[key] === 'unknown' ? 'unconfigured' : 'template';
    }
  }

  for (const key of ['streaming', 'image_input', 'tool_calls', 'tool_image_projection'] as const) fill(key);
  for (const key of ['auto', 'none'] as const) {
    const value: Support = parameters.tool_choice[key];
    let source = 'online';
    if (value === 'unknown') source = template.tool_choice[key] === 'unknown' ? 'unconfigured' : 'template';
    sources[`tool_choice.${key}`] = source;
    if (value === 'unknown') parameters.tool_choice[key] = template.tool_choice[key];
  }
  if (parameters.reasoning !== 'unsupported' && parameters.reasoning_mode !== 'unsupported') {
    fill('reasoning');
    fill('reasoning_mode');
    if (parameters.reasoning_efforts !== null) sources.reasoning_efforts = 'online';
    else {
      parameters.reasoning_efforts = structuredClone(template.reasoning_efforts);
      sources.reasoning_efforts = template.reasoning_efforts === null ? 'unconfigured' : 'template';
    }
    const fallback = template.default_reasoning_effort;
    if (
      parameters.default_reasoning_effort === null &&
      fallback !== null &&
      Object.hasOwn(parameters.reasoning_efforts ?? {}, fallback)
    ) {
      parameters.default_reasoning_effort = fallback;
      sources.default_reasoning_effort = 'template';
    }
  }
  return { parameters, sources };
}
