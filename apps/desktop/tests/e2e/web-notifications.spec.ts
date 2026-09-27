import { compatibilityHeaders } from '@ez-assistant/protocol';
import { expect, test } from '@playwright/test';

// 使用真实 Host/SSE、Chrome Notifications 与 Web Locks；OS 横幅与点击另做实包验证。
test('F03 全局完成通知、多标签去重、重连抑制及点击定位', async ({ context, page }) => {
  test.setTimeout(60_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = {
    ...compatibilityHeaders(),
    Authorization: `Bearer ${host.access_token}`,
    'Content-Type': 'application/json',
  };
  async function command(type: string, payload: object = {}) {
    const response = await fetch(`${host.base_url}/commands`, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        request_id: crypto.randomUUID(),
        command: { scope: 'runtime', payload: { type, payload } },
      }),
    });
    const result = await response.json();
    expect(response.status, JSON.stringify(result)).toBe(200);
    return result.result.payload.payload;
  }
  const issued = await fetch(`${host.base_url}/auth/login`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ method: 'desktop' }),
  });
  const { token } = await issued.json();
  await context.grantPermissions(['notifications']);
  await context.addInitScript(() => {
    const Original = window.Notification;
    const notifications: Notification[] = [];
    Object.assign(window, { notificationProbe: notifications });
    // 保留真实 Chrome 构造和权限，只记录实例以验证数量与页面点击路由。
    window.Notification = new Proxy(Original, {
      get: (constructor, property) => Reflect.get(constructor, property, constructor),
      construct: (constructor, args) => {
        const notification = Reflect.construct(constructor, args) as Notification;
        notifications.push(notification);
        return notification;
      },
    });
  });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.locator('[data-workspace-transition]')).toHaveAttribute('data-workspace-transition', 'complete');
  expect(await page.evaluate(() => Notification.permission)).toBe('granted');
  await page.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
  await page.getByRole('menuitem', { name: '设置', exact: true }).click();
  await page.getByRole('button', { name: '系统通知', exact: true }).click();
  await page.getByRole('button', { name: '启用通知', exact: true }).click();
  await expect(page.getByRole('button', { name: '关闭通知', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '关闭设置', exact: true }).click();
  const second = await context.newPage();
  await second.goto(host.base_url);
  await expect(second.locator('[data-workspace-transition]')).toHaveAttribute('data-workspace-transition', 'complete');
  const app = await command('get_application_snapshot');
  const created = await command('create_session', {
    title: 'F03 通知目标',
    model_selection: null,
    workspace_id: app.snapshot.value.workspaces[0].workspace_id,
  });
  await command('submit_input', {
    session_id: created.session.session_id,
    message: 'F03 finish notification',
    mode: 'normal',
    variant: 'build',
    attachment_ids: [],
    quotes: [],
  });
  const count = async () => {
    let total = 0;
    for (const tab of [page, second])
      total += await tab.evaluate(
        () => (window as unknown as { notificationProbe: Notification[] }).notificationProbe.length,
      );
    return total;
  };
  await expect.poll(count).toBe(1);
  const receiver = (await page.evaluate(
    () => (window as unknown as { notificationProbe: Notification[] }).notificationProbe.length,
  ))
    ? page
    : second;
  await receiver.evaluate(() =>
    (window as unknown as { notificationProbe: Notification[] }).notificationProbe[0]!.dispatchEvent(
      new Event('click'),
    ),
  );
  await expect(receiver.locator('header[aria-label="会话标题栏"]')).toContainText('F03 通知目标');
  await receiver.reload();
  await expect(receiver.locator('[data-workspace-transition]')).toHaveAttribute(
    'data-workspace-transition',
    'complete',
  );
  await expect.poll(count).toBe(0);
  const approval_session = await command('create_session', {
    title: 'F03 审批目标',
    model_selection: null,
    workspace_id: app.snapshot.value.workspaces[0].workspace_id,
  });
  const permission_scope = { type: 'session', payload: { session_id: approval_session.session.session_id } };
  const permissions = await command('get_permission_document', { scope: permission_scope });
  await command('replace_permission_document', {
    scope: { type: 'session', payload: { session_id: approval_session.session.session_id } },
    expected_revision: permissions.document.revision,
    document: {
      schema_version: 1,
      rules: [
        ...permissions.document.rules,
        {
          id: 'f03-write-ask',
          effect: 'ask',
          variants: ['build'],
          matcher: {
            type: 'file',
            payload: {
              operation: 'write',
              path: `${app.snapshot.value.workspaces[0].user_directory}/e2e-context-file.txt`,
              path_match: 'exact',
            },
          },
        },
      ],
    },
  });
  await command('submit_input', {
    session_id: approval_session.session.session_id,
    message: 'TOOL_CASE',
    mode: 'normal',
    variant: 'build',
    attachment_ids: [],
    quotes: [],
  });
  await expect.poll(count).toBe(1);
  const approval_receiver = (await page.evaluate(
    () => (window as unknown as { notificationProbe: Notification[] }).notificationProbe.length,
  ))
    ? page
    : second;
  await approval_receiver.evaluate(() => {
    const notification = (window as unknown as { notificationProbe: Notification[] }).notificationProbe[0]!;
    Object.assign(window, { lateNotificationClick: notification.onclick?.bind(notification) });
    notification.dispatchEvent(new Event('click'));
  });
  await expect(approval_receiver.locator('header[aria-label="会话标题栏"]')).toContainText('F03 审批目标');
  await expect(approval_receiver.getByRole('button', { name: '允许执行', exact: true })).toBeVisible();
  const pending = await command('get_session_view', { session_id: approval_session.session.session_id });
  expect(pending.snapshot.value.approvals.items.length).toBe(1);
  await command('decide_approval', {
    session_id: approval_session.session.session_id,
    approval_id: pending.snapshot.value.approvals.items[0].approval_id,
    decision: 'deny',
  });
  await approval_receiver.evaluate(() =>
    (window as unknown as { lateNotificationClick: (event: Event) => void }).lateNotificationClick(new Event('click')),
  );
  await expect(approval_receiver.getByText('这项审批已处理或已失效，当前显示会话最新状态。')).toBeVisible();
  await receiver.getByRole('button', { name: '账号与 Runtime', exact: true }).click();
  await receiver.getByRole('menuitem', { name: '设置', exact: true }).click();
  await receiver.getByRole('button', { name: '系统通知', exact: true }).click();
  await context.clearPermissions();
  await receiver.getByRole('button', { name: '刷新权限', exact: true }).click();
  await expect(receiver.getByRole('dialog', { name: '设置', exact: true }).getByRole('status')).toContainText(
    '尚未授权',
  );
  await expect(receiver.getByRole('button', { name: '发送测试通知', exact: true })).toBeDisabled();
});
