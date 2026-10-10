import type { RuntimeClient } from '../../runtime-client/RuntimeClient';
import type { SystemNotification } from './types';

/** 通知只摘录可见文字，不渲染 HTML，也不加载链接或图片资源。 */
export function notificationText(source: string, limit = 180): string {
  const text = source.slice(0, 16_000)
    .replace(/<!--[\s\S]*?(?:-->|$)/g, '')
    .replace(/<(script|style)\b[^>]*>[\s\S]*?(?:<\/\1>|$)/gi, '')
    .replace(/^\s*\[[^\]\n]+\]:\s+.*$/gm, '')
    .replace(/^\s*(?:`{3,}|~{3,})[^\n]*$/gm, '')
    .replace(/!?\[([^\]\n]*)\]\([^\n]*?\)/g, '$1')
    .replace(/!?\[([^\]\n]*)\]\[[^\]\n]*\]/g, '$1')
    .replace(/<[^>\n]*>/g, '')
    .replace(/^\s*(?:#{1,6}\s+|>\s*|[-+*]\s+|\d+[.)]\s+)/gm, '')
    .replace(/(`+)([^`]+)\1/g, '$2')
    .replace(/(\*{1,3}|~~)([\s\S]*?)\1/g, '$2')
    .replace(/(^|\s)_{1,3}([^_]+)_{1,3}(?=\s|[.,!?，。！？]|$)/g, '$1$2')
    .replace(/\\([\\`*{}[\]()#+.!_>~-])/g, '$1')
    .replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);/gi, (entity, name: string) => {
      if (!name.startsWith('#')) {
        return ({ amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' } as Record<string, string>)[name.toLowerCase()] ?? entity;
      }
      const value = name[1]?.toLowerCase() === 'x' ? Number.parseInt(name.slice(2), 16) : Number(name.slice(1));
      return value > 0 && value <= 0x10ffff && !(value >= 0xd800 && value <= 0xdfff)
        ? String.fromCodePoint(value) : '';
    })
    .replace(/[\p{Cc}\u202a-\u202e\u2066-\u2069]/gu, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  const characters = Array.from(text);
  return characters.length > limit ? `${characters.slice(0, limit - 1).join('')}…` : text;
}

/** 独立只读请求：不更新当前会话、分页缓存或实时水位。投递方在 await 后复核 scope。 */
export async function resolveNotificationContent(
  client: Pick<RuntimeClient, 'command'> | null,
  notification: SystemNotification,
): Promise<SystemNotification | null> {
  if (!client) return null;
  const { target } = notification;
  const result = await client.command({ type: 'get_session', payload: { session_id: target.owner.session_id } }).catch(() => null);
  if (!result) return null;
  const session = result.payload.session;
  if (session.session_id !== target.owner.session_id) return null;
  const title = notificationText(session.title, 64) || '未命名会话';
  const fallback = { ...notification, title, body: notificationText(notification.body) };
  if (target.kind !== 'turn' || target.owner.type !== 'main_session') return fallback;

  try {
    // 此查询锚定指定 Run 的最后可靠消息，不能改用当前尾页（可能已经有后继 Run）。
    const around = await client.command({
      type: 'get_conversation_page_around_run',
      payload: { session_id: target.owner.session_id, run_id: target.run_id, limit: 1 },
    });
    const page = around.payload.snapshot.value;
    if (page.owner.type !== 'main_session' || page.owner.session_id !== target.owner.session_id) return null;
    const message = page.items.find((item) => item.message_id === around.payload.anchor_message_id);
    if (!message) return null;
    if (message.type !== 'assistant') return fallback;
    if (message.run_id !== target.run_id) return null;
    const raw_body = message.segments
      .filter((segment) => segment.type === 'text')
      .map((segment) => segment.text).join('\n\n');
    if (!notificationText(raw_body)) return fallback;
    const prefix = message.status === 'completed' ? '' : `${notification.body} `;
    return { ...fallback, body: notificationText(`${prefix}${raw_body}`) };
  } catch {
    // 无可靠正文或定位不可用只降级到事件文案，不读取另一条消息，也不重发通知。
    return fallback;
  }
}
