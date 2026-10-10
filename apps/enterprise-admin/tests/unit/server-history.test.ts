import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';
import { serverHistorySupport } from '../../src/features/models/model';

it('管理员表单与个人模式使用同一上游能力投影，Chat 覆盖不能绕过禁用', () => {
  const fixtures = JSON.parse(readFileSync('../../packages/assistant-protocol/fixtures/server-history.json', 'utf8'));
  for (const fixture of fixtures)
    expect(
      serverHistorySupport({
        server_history_enabled: false,
        server_history_support_override: null,
        ...fixture.connection,
      }),
      fixture.name,
    ).toBe(fixture.expected);
});

it('WebSocket 自定义入口必须声明支持，不能套用 HTTP 默认能力', () => {
  const fixtures = JSON.parse(readFileSync('../../packages/assistant-protocol/fixtures/server-history.json', 'utf8'));
  const connection = {
    ...fixtures[0].connection,
    responses_transport: 'websocket',
    endpoint: 'https://proxy.example/v1',
    server_history_enabled: false,
    server_history_support_override: null,
  };
  expect(serverHistorySupport(connection)).toBe('unknown');
  expect(serverHistorySupport({ ...connection, server_history_support_override: 'supported' })).toBe('supported');
  expect(
    serverHistorySupport({
      ...connection,
      protocol_preference: 'chat_completions',
      server_history_support_override: 'supported',
    }),
  ).toBe('unsupported');
});
