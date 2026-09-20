import { describe, expect, it } from 'vitest';
import { AxiosError, AxiosHeaders } from 'axios';
import { failure } from '../../src/request/api';
import { passwordRule } from '../../src/features/passwords/validation';
import { auditView } from '../../src/model';

describe('正式契约', () => {
  it('未知服务端消息与秘密不进入错误提示', () => {
    const error = new AxiosError('secret', undefined, undefined, undefined, {
      data: { error: { code: 'UNKNOWN', message: 'password=secret', request_id: 'bad-id-secret' } },
      status: 500,
      statusText: 'Error',
      headers: {},
      config: { headers: new AxiosHeaders() },
    });
    expect(failure(error, true).text).toContain('结果未确认');
    expect(failure(error).text).not.toContain('secret');
  });
  it('审计详情只渲染白名单，忽略密码和任意字段', () => {
    const details = { before: {}, password: 'secret' };
    const view = auditView({
      id: 1,
      occurred_at: '2026-09-18T00:00:00Z',
      actor_user_id: null,
      target_user_id: null,
      actor_username: null,
      target_username: null,
      action: 'self_password_changed',
      success: true,
      reason_code: null,
      request_id: 'request',
      details,
    });
    expect(view.action).toBe('修改本人密码');
    expect(view.actor).toBe('未认证');
    expect(JSON.stringify(view)).not.toContain('secret');
  });
  it('审计优先显示关联用户名，无关联时不猜测失败登录账号', () => {
    const record = {
      id: 2,
      occurred_at: '2026-09-20T00:00:00Z',
      actor_user_id: 1,
      target_user_id: 50,
      actor_username: 'admin',
      target_username: 'alice',
      action: 'user_updated',
      success: true,
      reason_code: null,
      request_id: 'request',
      details: {},
    };
    expect(auditView(record)).toMatchObject({ actor: 'admin', target: 'alice' });
    expect(auditView({ ...record, actor_username: null, target_username: null })).toMatchObject({
      actor: '用户 #1',
      target: '用户 #50',
    });
    expect(
      auditView({ ...record, actor_user_id: null, target_user_id: null, actor_username: null, target_username: null }),
    ).toMatchObject({ actor: '未认证', target: '—' });
  });
  it('新密码至少六位且同时有英文字母和数字，允许附加其他字符', async () => {
    for (const password of ['abc123', 'ABC123', '中😀 a1b', `a1${'x'.repeat(126)}`])
      await expect(passwordRule.validator(undefined, password)).resolves.toBeUndefined();
    for (const password of ['ab123', 'abcdef', '123456', '中文数字123', '😀😀a1', `a1${'x'.repeat(127)}`])
      await expect(passwordRule.validator(undefined, password)).rejects.toThrow();
  });
});
