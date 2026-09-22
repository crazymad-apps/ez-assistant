import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';
import {
  emptyParameters,
  mergeParameters,
  parseParameters,
  validParameters,
  parseConnection,
  resolveProtocol,
} from '../../dist/models/parameters.js';
import { parseTemplates } from '../../dist/models/templates.js';
import { parsePage } from '../../dist/models/discovery.js';

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
it('连接按实例类型解析协议并拒绝跨源发现路径', () => {
  const connection = {
    display_name: 'test',
    provider_type: 'openai',
    endpoint: 'http://127.0.0.1:12345/v1',
    protocol_preference: 'auto',
    models_path: '',
    discovery_format: 'openai',
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
