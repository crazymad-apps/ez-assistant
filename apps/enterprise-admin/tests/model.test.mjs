import assert from 'node:assert/strict';
import test from 'node:test';
import { createDemoAudit, createDemoUsers, userChangeError } from '../prototype/model.ts';

test('演示数据每次生成独立副本、账号唯一、超级管理员具有管理员角色', () => {
  const users = createDemoUsers();
  assert.equal(users.length, 12);
  assert.equal(new Set(users.map((user) => user.username)).size, users.length);
  assert.equal(users.find((user) => user.superAdmin).role, 'admin');
  users.pop();
  assert.equal(createDemoUsers().length, 12);
});

test('未决超级管理员停用和降权仅提示未模拟', () => {
  const users = createDemoUsers();
  const user = users.find((item) => item.superAdmin);
  assert.match(userChangeError(users, user, { ...user, enabled: false }), /尚待确认/);
  assert.match(userChangeError(users, user, { ...user, role: 'user' }), /尚待确认/);
  assert.equal(userChangeError(users, user, { ...user, displayName: '演示修改' }), undefined);
});

test('普通用户启停不受最后管理员保护影响', () => {
  const users = createDemoUsers();
  const user = users.find((item) => item.role === 'user');
  assert.equal(userChangeError(users, user, { ...user, enabled: false }), undefined);
});

test('最后管理员的演示保护不允许停用或降权', () => {
  const user = createDemoUsers().find((item) => item.role === 'admin' && !item.superAdmin);
  assert.match(userChangeError([user], user, { ...user, enabled: false }), /至少需要/);
  assert.match(userChangeError([user], user, { ...user, role: 'user' }), /至少需要/);
});

test('审计示例区分两个改密口径且不包含演示密码', () => {
  const records = createDemoAudit();
  assert.ok(records.some((record) => record.action === '管理员重置密码'));
  assert.ok(records.some((record) => record.action === '修改本人密码'));
  assert.ok(!JSON.stringify(records).includes('Demo@'));
});
