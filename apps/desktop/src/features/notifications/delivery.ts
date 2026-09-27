/** 浏览器标签页仅协调投递回执，不保存会话、正文或待办状态。 */
export async function deliverOnce(key: string, enabled: () => boolean, send: () => Promise<void>): Promise<void> {
  if (!navigator.locks || !globalThis.crypto?.subtle) throw new Error('浏览器不支持通知去重，请更新 Chrome 后重试。');
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(key));
  const digest = Array.from(new Uint8Array(bytes), (value) => value.toString(16).padStart(2, '0')).join('');
  await navigator.locks.request('ez-notification-delivery', async () => {
    if (!enabled()) return;
    const storage_key = 'ez-assistant:notification-receipts';
    const raw = localStorage.getItem(storage_key);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    const receipts = Array.isArray(parsed)
      ? parsed.filter((item): item is string => typeof item === 'string').slice(-1024)
      : [];
    if (receipts.includes(digest)) return;
    // 先写回执再发送：崩溃时允许漏提醒，不能因另一标签页接管而重复打扰。
    localStorage.setItem(storage_key, JSON.stringify([...receipts.slice(-1023), digest]));
    await send();
  });
}
