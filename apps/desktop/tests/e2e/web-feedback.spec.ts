import { compatibilityHeaders } from '@ez-assistant/protocol';
import { expect, test } from '@playwright/test';
import { join } from 'node:path';

test('F04 真实 Host 问答、通知定位、提交失败、轮后恢复与取消', async ({ context, page }) => {
  test.setTimeout(90_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, 'Content-Type': 'application/json' };
  async function command(type: string, payload: object = {}) {
    const response = await fetch(`${host.base_url}/commands`, { method: 'POST', headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }) });
    const body = await response.json(); expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  const login = await fetch(`${host.base_url}/auth/login`, { method: 'POST', headers, body: JSON.stringify({ method: 'desktop' }) });
  const { token } = await login.json();
  await context.grantPermissions(['notifications']);
  await context.addInitScript(() => {
    localStorage.setItem('ez-assistant:system-notifications', 'true');
    const Original = window.Notification; const notifications: Notification[] = [];
    Object.assign(window, { feedbackNotifications: notifications });
    window.Notification = new Proxy(Original, {
      get: (constructor, property) => Reflect.get(constructor, property, constructor),
      construct: (constructor, args) => { const item = Reflect.construct(constructor, args) as Notification; notifications.push(item); return item; },
    });
  });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.locator('[data-workspace-transition]')).toHaveAttribute('data-workspace-transition', 'complete');
  const created = await command('create_session', { title: 'F04 问答验收' }); const session_id = created.session.session_id;
  const notifications = () => page.evaluate(() => (window as unknown as { feedbackNotifications: Notification[] }).feedbackNotifications.length);
  async function submit(message: string) { return command('submit_input', { session_id, message, mode: 'normal', variant: 'build', attachment_ids: [], quotes: [] }); }
  await page.getByText('F04 问答验收', { exact: true }).first().click();
  await page.getByRole('textbox', { name: '输入消息' }).fill('普通草稿保留');
  await command('create_session', { title: 'F04 背景会话' });
  await page.getByText('F04 背景会话', { exact: true }).first().click();
  const first = await submit('F04_WAIT');
  await expect.poll(notifications).toBe(1);
  await page.evaluate(() => (window as unknown as { feedbackNotifications: Notification[] }).feedbackNotifications[0].dispatchEvent(new Event('click')));
  const panel = page.getByRole('region', { name: '等待你回答' });
  await expect(panel).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "等待回复" })).toBeVisible();
  await expect(panel.getByRole('radio', { name: /PDF/ })).not.toBeChecked();
  await expect(page.getByRole('textbox', { name: '输入消息' })).toHaveCount(0);
  await expect(panel.getByRole('button', { name: '收起' })).toHaveCount(0);
  await panel.getByRole('radio', { name: /PDF/ }).click();
  await panel.getByRole('textbox').fill('保留源文件');
  await panel.getByRole('button', { name: '下一题' }).click();
  await expect(panel.getByRole('checkbox', { name: '说明' })).toBeFocused();
  await panel.getByRole('checkbox', { name: '说明' }).click();
  await panel.getByRole('checkbox', { name: '清单' }).click();
  await panel.getByRole('button', { name: /格式/ }).click();
  await expect(panel.getByRole('textbox')).toHaveValue('保留源文件');
  await page.screenshot({ path: join(process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME!, 'feedback-options.png'), fullPage: true });
  await panel.screenshot({ path: join(process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME!, 'feedback-detail.png') });
  await page.setViewportSize({ width: 1024, height: 700 });
  const bounds = await panel.boundingBox();
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(700);
  await expect(panel.getByRole('button', { name: '取消' })).toBeInViewport();
  await page.screenshot({ path: join(process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME!, 'feedback-compact.png'), fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await panel.getByRole('button', { name: '补充' }).click();
  await panel.getByRole('textbox').fill('中文补充回答');
  await page.screenshot({ path: join(process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME!, 'feedback-panel.png'), fullPage: true });
  let fail = true;
  await page.route('**/commands', async (route) => {
    if (fail && route.request().postDataJSON()?.command?.payload?.type === 'answer_feedback') { fail = false; await route.abort('failed'); }
    else await route.continue();
  });
  await panel.getByRole('button', { name: '回复并继续' }).click();
  await expect(panel.getByRole('alert')).toBeVisible();
  await expect(panel.getByRole('textbox')).toHaveValue('中文补充回答');
  await panel.getByRole('button', { name: '回复并继续' }).click();
  await expect(panel).toHaveCount(0);
  await expect(page.getByRole('textbox', { name: '输入消息' })).toHaveValue('普通草稿保留');
  await expect.poll(async () => (await command('get_run', { session_id, run_id: first.run.run_id })).run.status).toBe('completed');
  await expect.poll(notifications).toBe(2);
  const end = await submit('F04_END');
  await expect(panel).toBeVisible();
  await expect.poll(async () => (await command('get_run', { session_id, run_id: end.run.run_id })).run.status).toBe('completed');
  await expect.poll(notifications).toBe(3);
  await page.reload();
  await expect(panel).toBeVisible();
  expect(await notifications()).toBe(0);
  await expect(page.getByRole('textbox', { name: '输入消息' })).toHaveCount(0);
  await expect(panel.getByRole('button', { name: '收起' })).toHaveCount(0);
  await panel.getByRole('button', { name: '取消' }).click();
  await expect(panel).toHaveCount(0);
  const feedback = await command('list_feedback_requests', { session_id });
  expect(feedback.map((item: { status: string }) => item.status).sort()).toEqual(['answered', 'cancelled']);
  const cancelled = await submit('F04_WAIT');
  await expect(panel).toBeVisible();
  await expect(page.getByRole('textbox', { name: '输入消息' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '取消等待并发送新指令' })).toHaveCount(0);
  await panel.getByRole('button', { name: '取消' }).click();
  await expect(panel).toHaveCount(0);
  await expect.poll(async () => (await command('get_run', { session_id, run_id: cancelled.run.run_id })).run.status).toBe('cancelled');
  await expect(page.getByRole('textbox', { name: '输入消息' })).toBeVisible();
  await page.getByRole('textbox', { name: '输入消息' }).fill('取消后新指令');
  await page.getByRole('textbox', { name: '输入消息' }).press('Enter');
  await expect(page.getByRole('textbox', { name: '输入消息' })).toHaveValue('');
  await expect(page.getByText('已取消', { exact: true }).first()).toBeVisible();
});
