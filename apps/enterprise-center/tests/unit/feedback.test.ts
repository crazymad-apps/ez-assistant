import { randomUUID } from 'node:crypto';
import { expect, it } from 'vitest';
import { feedbackQuery, handling, submission } from '../../dist/feedback/validation.js';

const input = () => ({ submission_id: randomUUID(), source: 'agent', title: ' 问题 ', description: ' 第一行\n第二行 ',
  target: { kind: 'knowledge', name: '手动描述的知识库' }, environment: { client_kind: 'agent' } });
it('规范化保留正文换行、空引用和首次环境语义，字符限制按 Unicode 计数', () => {
  const raw = input();
  const result = submission(raw);
  expect(result.title).toBe('问题');
  expect(result.description).toBe('第一行\n第二行');
  expect(result.target.reference).toBeNull();
  expect(result.environment.host_os).toBeNull();
  expect(submission({ ...raw, title: '😀'.repeat(120) }).title).toHaveLength(240);
  expect(() => submission({ ...raw, title: '😀'.repeat(121) })).toThrow();
  expect(() => submission({ ...raw, title: '\ud800' })).toThrow();
  expect(() => submission({ ...raw, title: 'a\0b' })).toThrow();
  expect(() => submission({ ...raw, description: '中'.repeat(4000), reproduction: '中'.repeat(1000) })).toThrowError(expect.objectContaining({ code: 'FEEDBACK_TOO_LARGE' }));
});
it('目标组合与固定环境拒绝非法和越界字段', () => {
  const raw = input();
  for (const target of [
    { kind: 'plugin', name: '插件' }, { kind: 'plugin', name: '插件', reference: '../x' },
    { kind: 'platform', name: '平台', reference: 'another' }, { ...raw.target, version: '1.0' },
    { ...raw.target, url: 'https://example.invalid' },
  ]) expect(() => submission({ ...raw, target })).toThrow();
  expect(() => submission({ ...raw, environment: { ...raw.environment, token: 'private' } })).toThrow();
  expect(() => submission({ ...raw, environment: { client_kind: 'agent', host_os: '中'.repeat(128), host_arch: '中'.repeat(128),
    host_version: '中'.repeat(128), client_version: '中'.repeat(128), client_os: '中'.repeat(128), client_arch: '中'.repeat(128) } })).toThrowError(expect.objectContaining({ code: 'FEEDBACK_TOO_LARGE' }));
});
it('本人列表不能传归属覆盖，目标引用必须具备类型，终结处理须有说明', () => {
  expect(() => feedbackQuery({ owner_user_id: '1' }, false)).toThrow();
  expect(() => feedbackQuery({ target_reference: '123' }, true)).toThrow();
  expect(() => feedbackQuery({ target_kind: 'knowledge', target_reference: '123', target_unresolved: 'true' }, true)).toThrow();
  expect(feedbackQuery({ target_kind: 'knowledge', target_unresolved: 'false' }, true).unresolved).toBe(false);
  expect(() => handling({ expected_status: 'pending', expected_handling_note: null, status: 'closed', handling_note: '\n' })).toThrow();
});
