import { readFileSync } from 'node:fs';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { expect, it } from 'vitest';
import {
  emptyParameters,
  mergeParameters,
  parseParameters,
  validParameters,
  parseConnection,
  resolveProtocol,
  executable,
} from '../../dist/models/parameters.js';
import { parseTemplates } from '../../dist/models/templates.js';
import { discover, parsePage } from '../../dist/models/discovery.js';

const asset = JSON.parse(readFileSync('../../packages/assistant-protocol/resources/model-templates.json', 'utf8'));
it('共享静态模板全部可校验，重复身份和未知字段拒绝整份加载', () => {
  const entries = parseTemplates(asset);
  expect(entries.length).toBeGreaterThan(0);
  expect(() => parseTemplates([...asset, asset[0]])).toThrow();
  expect(() => parseTemplates([{ ...asset[0], expression: 'true' }])).toThrow();
  expect(() => parseTemplates([{ ...asset[0], parameters: { ...asset[0].parameters, streaming: true } }])).toThrow();
  expect(() =>
    parseTemplates([{ ...asset[0], parameters: { tool_calls: 'unsupported', tool_choice: { auto: 'supported' } } }]),
  ).toThrow();
});
it('GPT-6 Sol 模板只在 Responses 声明当前可用的工具调用能力', () => {
  const template = parseTemplates(asset).find((item) => item.provider_type === 'openai' && item.model_id === 'gpt-6-sol')!;
  expect(template.parameters.context_window_tokens).toEqual({ state: 'known', value: 1050000 });
  expect(template.parameters.max_output_tokens).toEqual({ state: 'known', value: 128000 });
  expect(template.parameters.default_reasoning_effort).toBe('medium');
  expect(template.parameters.reasoning_efforts).toEqual({
    low: 'low',
    medium: 'medium',
    high: 'high',
    xhigh: 'xhigh',
    max: 'max',
  });
  expect(template.protocol_parameters.open_ai_responses?.tool_calls).toBe('supported');
  expect(template.protocol_parameters.open_ai_responses?.tool_image_projection).toBe('native_tool_result');
  expect(template.protocol_parameters.open_ai_chat_completions?.tool_calls).toBe('unsupported');
  expect(template.protocol_parameters.open_ai_chat_completions?.tool_image_projection).toBe('unsupported');
});
it('在线非法值、明确禁用思考和空档位映射不被模板掩盖', () => {
  const template = parseTemplates(asset).find((item) => item.model_id === 'gpt-6-astra')!.parameters;
  const online = emptyParameters();
  online.context_window_tokens = { state: 'invalid' };
  online.image_input = 'unsupported';
  online.reasoning_efforts = {};
  const result = mergeParameters(online, template);
  expect(result.parameters.context_window_tokens).toEqual({ state: 'invalid' });
  expect(result.parameters.image_input).toBe('unsupported');
  expect(result.parameters.reasoning_efforts).toEqual({});
  expect(result.parameters.default_reasoning_effort).toBeNull();
  expect(result.sources.context_window_tokens).toBe('online');
  expect(validParameters(result.parameters, true)).toBe(false);
  online.reasoning = 'unsupported';
  const disabled = mergeParameters(online, template).parameters;
  expect(disabled.reasoning_mode).toBe('unknown');
  expect(disabled.reasoning_efforts).toEqual({});
  expect(parseParameters({ reasoning_efforts: null }, true).reasoning_efforts).toBeNull();
  expect(parseParameters({ reasoning_efforts: {} }, true).reasoning_efforts).toEqual({});
});
it('发现格式不会从名称或普通 OpenAI 扩展猜测能力，Moonshot 空档位保留', () => {
  const model = { object: 'model', id: 'x', max_model_len: 8192 };
  expect(
    parsePage({ object: 'list', data: [model] }, 'openai', 1).models[0]!.metadata.context_window_tokens.state,
  ).toBe('unknown');
  expect(parsePage({ object: 'list', data: [model] }, 'vllm', 1).models[0]!.metadata.context_window_tokens).toEqual({
    state: 'known',
    value: 8192,
  });
  const moon = parsePage(
    { object: 'list', data: [{ ...model, supports_reasoning: true, think_efforts: { support: false } }] },
    'moonshot',
    1,
  );
  expect(moon.models[0]!.metadata.reasoning_efforts).toEqual({});
  expect(() => parsePage({ object: 'list', has_more: true, data: [] }, 'openai', 1)).toThrow();
});
it('OpenAI 兼容目录允许条目省略 object，不从附加字段推断能力', () => {
  const result = parsePage(
    {
      object: 'list',
      data: [
        {
          id: 'compatible-model', type: 'model', display_name: 'Compatible',
          created_at: '2026-01-01', max_model_len: 8192,
        },
        { id: 'standard-model', object: 'model' },
      ],
    },
    'openai',
    1,
  );
  expect(result.models.map((model) => model.model_id)).toEqual(['compatible-model', 'standard-model']);
  expect(result.models[0]!.metadata).toEqual(emptyParameters());
});

it.each([null, 'list', '', 1, false, {}])('目录条目显式声明非法 object 时仍拒绝：%j', (object) => {
  expect(() => parsePage({ object: 'list', data: [{ id: 'model', object }] }, 'openai', 1)).toThrow();
});

it.each([undefined, null, '', ' spaced ', 'bad\nmodel', 1, 'x'.repeat(513)])(
  '省略 object 不放宽模型 ID 校验：%j',
  (id) => {
    expect(() => parsePage({ object: 'list', data: [{ id }] }, 'openai', 1)).toThrow();
  },
);

it('完整发现接受省略 object 的目录，但仍拒绝重复模型', async () => {
  let document = { object: 'list', data: [{ id: 'compatible-model' }] };
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify(document));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('expected loopback address');
    const connection = parseConnection({
      display_name: 'fixture', provider_type: 'openai', endpoint: `http://127.0.0.1:${address.port}/v1`,
      protocol_preference: 'responses', models_path: '/v1/models', discovery_format: 'openai',
      server_history_enabled: false, server_history_support_override: null,
    });
    const models = await discover(connection, 'fixture-key', new AbortController().signal);
    expect(models.map((model) => model.model_id)).toEqual(['compatible-model']);
    document = { ...document, data: [{ id: 'duplicate' }, { id: 'duplicate' }] };
    await expect(discover(connection, 'fixture-key', new AbortController().signal)).rejects.toMatchObject({
      code: 'MODEL_DISCOVERY_FAILED',
    });
  } finally {
    await new Promise<void>((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  }
});

it('连接按实例类型解析协议并拒绝跨源发现路径', () => {
  const connection = {
    display_name: 'test',
    provider_type: 'openai',
    endpoint: 'http://127.0.0.1:12345/v1',
    protocol_preference: 'auto',
    models_path: '',
    discovery_format: 'openai', server_history_enabled: false, server_history_support_override: null,
  };
  expect(resolveProtocol(parseConnection(connection))).toBe('open_ai_responses');
  expect(() => parseConnection({ ...connection, models_path: '//evil.test/models' })).toThrow();
  expect(() => parseConnection({ ...connection, endpoint: 'https://user:secret@example.test' })).toThrow();
});

it('标准档位只接受 xhigh，拒绝旧拼写的映射键和默认值', () => {
  const parameters = parseParameters(
    { reasoning_efforts: { xhigh: 'vendor-value' }, default_reasoning_effort: 'xhigh' },
    true,
  );
  expect(parameters.reasoning_efforts).toEqual({ xhigh: 'vendor-value' });
  expect(parameters.default_reasoning_effort).toBe('xhigh');
  expect(() => parseParameters({ reasoning_efforts: { x_high: 'vendor-value' } }, true)).toThrow();
  expect(() => parseParameters({ default_reasoning_effort: 'x_high' }, true)).toThrow();
});

it('共享参数样例约束 Node/Rust 的协议覆盖和补未知语义', () => {
  const vectors = JSON.parse(readFileSync('../../packages/assistant-protocol/fixtures/model-parameters.json', 'utf8'));
  const entry = parseTemplates(asset).find(
    (item) => item.provider_type === 'openai' && item.model_id === 'gpt-6-astra',
  )!;
  for (const vector of vectors) {
    const protocol = vector.protocol_preference === 'responses' ? 'open_ai_responses' : 'open_ai_chat_completions';
    const result = mergeParameters(
      parseParameters(vector.online, true),
      entry.protocol_parameters[protocol] ?? entry.parameters,
    );
    for (const [key, expected] of Object.entries(vector.expected))
      expect(Reflect.get(result.parameters, key), vector.name).toEqual(expected);
    expect(result.sources, vector.name).toMatchObject(vector.sources);
  }
});

it('OpenCode 使用显式协议、独立列表路径，不继承模型名称或续接/WS 能力', () => {
  const connection = {
    display_name: 'OpenCode', provider_type: 'opencode',
    endpoint: 'https://opencode.ai/inference/go/openai/v1',
    models_path: '/inference/go/v1/models', discovery_format: 'openai',
    protocol_preference: 'chat_completions', responses_transport: 'http',
    server_history_enabled: false, server_history_support_override: null,
  };
  expect(resolveProtocol(parseConnection(connection))).toBe('open_ai_chat_completions');
  expect(resolveProtocol(parseConnection({ ...connection, protocol_preference: 'responses' }))).toBe('open_ai_responses');
  for (const patch of [
    { protocol_preference: 'auto' }, { responses_transport: 'websocket', protocol_preference: 'responses' },
    { server_history_enabled: true }, { server_history_support_override: 'supported' },
    { provider_type: 'future-provider' }, { models_path: 'https://other.test/models' },
  ]) expect(() => parseConnection({ ...connection, ...patch })).toThrow();

  const parameters = { ...emptyParameters(), context_window_tokens: { state: 'known', value: 8192 } as const,
    max_output_tokens: { state: 'known', value: 1024 } as const, streaming: 'supported' as const };
  const responses = parseConnection({ ...connection, protocol_preference: 'responses' });
  expect(executable(responses, parameters)).toBe(true);
  expect(executable(responses, { ...parameters, input_token_count_support: 'supported' })).toBe(false);
  expect(executable(responses, { ...parameters, server_compaction_support: 'supported', server_compaction_enabled: true })).toBe(false);
  const template = { ...asset[0], provider_type: 'opencode' };
  expect(parseTemplates([template])[0]?.provider_type).toBe('opencode');
});

it('Sub2API 增强目录只映射已声明能力，不执行提示词或混淆并行与工具能力', async () => {
  const { parseSub2api } = await import('../../dist/models/sub2api.js');
  const vectors = JSON.parse(readFileSync('../../packages/assistant-protocol/fixtures/sub2api-discovery.json', 'utf8'));
  for (const vector of vectors) {
    if (vector.document.models === undefined) continue;
    if (vector.invalid) expect(() => parseSub2api(vector.document), vector.name).toThrow();
    else expect(parseSub2api(vector.document)[0]!.metadata, vector.name).toMatchObject(vector.expected);
  }
});

it('Sub2API 发现追加增强开关，只有明确不支持才回退同源普通路径', async () => {
  const seen: string[] = [];
  let mode = 'fallback';
  const server = createServer((request, response) => {
    seen.push(request.url!);
    if (mode === 'fallback' && request.url!.includes('?')) {
      response.writeHead(404).end();
    } else if (mode === 'unauthorized') response.writeHead(401).end();
    else {
      response.writeHead(200, { 'Content-Type': 'application/json' });
      if (mode === 'invalid') response.end(JSON.stringify({ models: [{ slug: ' bad ' }] }));
      else if (mode === 'enhanced') response.end(JSON.stringify({ models: [{
        slug: 'enhanced', context_window: 272000,
        model_messages: { instructions_template: 'x'.repeat(1024 * 1024 + 1) },
      }] }));
      else response.end(JSON.stringify({ object: 'list', data: [{ id: 'ordinary' }] }));
    }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('expected loopback address');
    const connection = parseConnection({
      display_name: 'Sub2API', provider_type: 'sub2api', endpoint: `http://127.0.0.1:${address.port}/v1`,
      protocol_preference: 'auto', models_path: '', discovery_format: 'sub2api',
      server_history_enabled: false, server_history_support_override: null,
    });
    expect(resolveProtocol(connection)).toBe('open_ai_responses');
    const load = () => discover(connection, 'fixture-key', new AbortController().signal);
    expect((await load()).map((model) => model.model_id)).toEqual(['ordinary']);
    expect(seen.splice(0)).toEqual(['/v1/models?client_version=ez-assistant', '/v1/models']);
    mode = 'enhanced';
    expect((await load())[0]!.metadata.context_window_tokens).toEqual({ state: 'known', value: 272000 });
    expect(seen.splice(0)).toEqual(['/v1/models?client_version=ez-assistant']);
    for (mode of ['unauthorized', 'invalid']) {
      await expect(load()).rejects.toMatchObject({ code: 'MODEL_DISCOVERY_FAILED' });
      expect(seen.splice(0)).toEqual(['/v1/models?client_version=ez-assistant']);
    }
  } finally {
    await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

it('输出上限未知和流式缺省不再阻止启用，非法限制和明确不支持仍拒绝', () => {
  const connection = parseConnection({
    display_name: 'Sub2API', provider_type: 'sub2api', endpoint: 'https://example.test/v1',
    protocol_preference: 'auto', models_path: '', discovery_format: 'sub2api',
    server_history_enabled: false, server_history_support_override: null,
  });
  const parameters = emptyParameters();
  parameters.context_window_tokens = { state: 'known', value: 272000 };
  expect(validParameters(parameters, true)).toBe(true);
  expect(executable(connection, parameters)).toBe(true);
  parameters.streaming = 'unsupported';
  expect(executable(connection, parameters)).toBe(false);
  parameters.streaming = 'unknown';
  parameters.max_output_tokens = { state: 'invalid' };
  expect(executable(connection, parameters)).toBe(false);
});
