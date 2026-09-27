import { Channel, invoke, isTauri } from '@tauri-apps/api/core';
import type { NotificationPermission, NotificationPlatform } from '../features/notifications';

export function createNotificationPlatform(): NotificationPlatform {
  const desktop = isTauri();
  return {
    desktop,

    async permission(request) {
      if (desktop) return invoke<NotificationPermission>('system_notification_permission', { request });
      if (!window.isSecureContext) return 'insecure';
      if (!('Notification' in window)) return 'unsupported';
      if (request && Notification.permission === 'default') return Notification.requestPermission();
      return Notification.permission;
    },

    async show(id, title, body, click, fail) {
      if (desktop) {
        return new Promise<() => void>((resolve, reject) => {
          let submitted = false;
          const events = new Channel<'submitted' | 'clicked'>();
          events.onmessage = (event) => {
            if (event === 'clicked') click();
            if (event === 'submitted') {
              submitted = true;
              resolve(() => {
                void invoke('close_system_notification', { id }).catch(fail);
              });
            }
          };
          void invoke('show_system_notification', { id, title, body, events })
            .then(() => {
              if (!submitted) reject(new Error('通知已取消。'));
            })
            .catch((error: unknown) => {
              if (submitted) fail();
              else reject(error);
            });
        });
      }
      const notification = new Notification(title, { body, tag: id });
      notification.onclick = () => {
        window.focus();
        click();
        notification.close();
      };
      notification.onerror = fail;
      return () => {
        notification.onclick = null;
        notification.onerror = null;
        notification.close();
      };
    },

    async focus() {
      if (desktop) await invoke('show_desktop_window');
      else window.focus();
    },
  };
}
