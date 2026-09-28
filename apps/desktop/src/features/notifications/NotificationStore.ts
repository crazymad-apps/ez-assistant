import { action, makeObservable, observable, observableRef, runInAction } from 'mobx';
import type { RuntimeEventEnvelope } from '@ez-assistant/protocol';
import { createNotificationPlatform } from '../../native-bridge/systemNotifications';
import { deliverOnce } from './delivery';
import {
  notificationFromEvent,
  notificationKey,
  type NotificationPermission,
  type NotificationPlatform,
  type NotificationTarget,
  type SystemNotification,
} from './types';

const PREFERENCE_KEY = 'ez-assistant:system-notifications';
type Dependencies = Readonly<{
  scope: () => string | null;
  navigate: (target: NotificationTarget) => Promise<boolean | void>;
}>;

/** RootStore 拥有的客户端投递状态；业务状态始终在点击时从 Runtime 重读。 */
export class NotificationStore {
  enabled = false;
  permission: NotificationPermission = 'default';
  busy = false;
  error_message: string | null = null;
  notice_message: string | null = null;
  focus_target: NotificationTarget | null = null;
  #disposed = false;
  #generation = 0;
  #activation = 0;
  #test_delay: { timer: number; finish: () => void } | null = null;
  #seen = new Set<string>();
  #withdrawn = new Set<string>();
  #handles = new Map<string, () => void>();
  #feedback_runs = new Set<string>();
  #feedback_navigate: ((target: NotificationTarget & { kind: 'feedback' }) => Promise<void>) | null = null;
  readonly #onFocus = () => {
    void this.refreshPermission();
  };
  readonly #onStorage = (event: StorageEvent) => {
    if (event.key === PREFERENCE_KEY || event.key === null) this.setEnabled(event.newValue === 'true', false);
  };

  constructor(
    private readonly dependencies: Dependencies,
    readonly platform: NotificationPlatform = createNotificationPlatform(),
  ) {
    makeObservable(this, {
      enabled: observable,
      permission: observable,
      busy: observable,
      error_message: observable,
      notice_message: observable,
      focus_target: observableRef,
      setEnabled: action,
      dismissNotice: action,
      consumeFocus: action,
    });
    try {
      this.enabled = localStorage.getItem(PREFERENCE_KEY) === 'true';
    } catch {
      /* 存储受限时默认关闭。 */
    }
    window.addEventListener('focus', this.#onFocus);
    window.addEventListener('storage', this.#onStorage);
  }

  async refreshPermission(): Promise<void> {
    try {
      const permission = await this.platform.permission(false);
      if (!this.#disposed)
        runInAction(() => {
          this.permission = permission;
        });
    } catch {
      if (!this.#disposed)
        runInAction(() => {
          this.permission = 'unavailable';
        });
    }
  }

  async enable(): Promise<void> {
    if (this.busy || this.#disposed) return;
    const generation = this.#generation;
    runInAction(() => {
      this.busy = true;
      this.error_message = null;
    });
    try {
      // 必须由设置按钮的用户手势直接调用，不能先等待其他异步工作。
      const permission = await this.platform.permission(true);
      if (this.#disposed || generation !== this.#generation) return;
      runInAction(() => {
        this.permission = permission;
        this.setEnabled(permission === 'granted');
      });
    } catch {
      this.#failure('无法申请系统通知权限，请检查系统设置或应用安装状态。');
    } finally {
      if (!this.#disposed)
        runInAction(() => {
          this.busy = false;
        });
    }
  }

  setEnabled(enabled: boolean, persist = true): void {
    this.enabled = enabled;
    this.#generation += 1;
    if (!enabled) this.#closeAll();
    if (persist) {
      try {
        localStorage.setItem(PREFERENCE_KEY, String(enabled));
      } catch {
        this.error_message = '通知开关未能保存，本次页面内仍然有效。';
      }
    }
  }

  ingest(envelope: RuntimeEventEnvelope): void {
    const scope = this.dependencies.scope();
    if (!scope || this.#disposed) return;
    const event = envelope.event;
    if (event.type === 'feedback_published' || event.type === 'feedback_changed') {
      const request = event.request;
      const target = { scope, kind: 'feedback' as const, request_id: request.request_id,
        run_id: request.run_id, owner: { type: 'main_session' as const, session_id: request.session_id } };
      if (event.type === 'feedback_published') this.feedbackPublished(target, request.mode === 'end_turn');
      else this.resolved(target);
      return;
    }
    if (event.type === 'approval_resolved' || event.type === 'approval_cancelled') {
      this.resolved({
        scope,
        kind: 'approval',
        approval_id: event.approval_id,
        run_id: event.run_id,
        owner: event.child_task_id
          ? { type: 'child_task', session_id: event.session_id, child_task_id: event.child_task_id }
          : { type: 'main_session', session_id: event.session_id },
      });
      return;
    }
    // 生命周期协调器只转发水位之后的新事件；快照和重连历史不会进入这里。
    // 不比较 Host 与客户端时钟，远端机器可能有时钟偏差。
    if (event.type === 'run_finished' && event.status === 'completed' && (event.awaiting_feedback || this.#feedback_runs.has(JSON.stringify([scope, event.run_id])))) return;
    const notification = notificationFromEvent(scope, envelope);
    if (notification) void this.publish(notification);
  }

  /** F04 必须注册重读权威问题并定位面板的回调；不能由通知层伪造等待状态。 */
  registerFeedbackNavigator(
    navigate: (target: NotificationTarget & { kind: 'feedback' }) => Promise<void>,
  ): () => void {
    this.#feedback_navigate = navigate;
    return () => {
      if (this.#feedback_navigate === navigate) this.#feedback_navigate = null;
    };
  }

  feedbackPublished(target: NotificationTarget & { kind: 'feedback' }, suppress_completion = true): void {
    if (target.scope !== this.dependencies.scope() || this.#disposed) return;
    if (suppress_completion) this.#feedback_runs.add(JSON.stringify([target.scope, target.run_id]));
    trimSet(this.#feedback_runs);
    void this.publish({ target, title: '等待你的回复', body: '点击返回会话回答问题。' });
  }

  resolved(target: NotificationTarget): void {
    const key = notificationKey(target);
    this.#withdrawn.add(key);
    trimSet(this.#withdrawn);
    this.#handles.get(key)?.();
    this.#handles.delete(key);
  }

  async publish(notification: SystemNotification): Promise<void> {
    const key = notificationKey(notification.target);
    if (this.#seen.has(key) || notification.target.scope !== this.dependencies.scope() || this.#disposed) return;
    this.#seen.add(key);
    trimSet(this.#seen);
    if (!this.enabled) return;
    const generation = this.#generation;
    const active = () =>
      !this.#withdrawn.has(key) &&
      !this.#disposed &&
      this.enabled &&
      generation === this.#generation &&
      notification.target.scope === this.dependencies.scope();
    try {
      const permission = await this.platform.permission(false);
      if (!active()) return;
      runInAction(() => {
        this.permission = permission;
      });
      if (permission !== 'granted') return;
      const send = async () => {
        // 发送前读取真实窗口焦点；前台收到的事项已消费，失焦后不补发。
        if (!active() || await this.platform.isFocused() || !active()) return;
        const close = await this.platform.show(
          crypto.randomUUID(),
          notification.title,
          notification.body,
          () => {
            void this.#activate(notification.target);
          },
          () => this.#failure('系统未能显示通知，请检查通知权限。'),
        );
        if (!active()) {
          close();
          return;
        }
        this.#handles.set(key, close);
        if (this.#handles.size > 32) {
          const oldest = this.#handles.entries().next().value;
          if (oldest) {
            oldest[1]();
            this.#handles.delete(oldest[0]);
          }
        }
      };
      if (this.platform.desktop) await send();
      else await deliverOnce(key, active, send);
    } catch (error) {
      if (active())
        this.#failure(error instanceof Error ? error.message : '系统通知发送失败，请检查通知权限或应用安装状态。');
    }
  }

  async test(delayed = false): Promise<void> {
    if (!this.enabled || this.busy || this.#disposed) return;
    const generation = this.#generation;
    runInAction(() => {
      this.busy = true;
      this.error_message = null;
      this.notice_message = null;
    });
    try {
      if (delayed) {
        runInAction(() => {
          this.notice_message = '将在 5 秒后发送测试通知，可以切换到其他窗口。';
        });
        await this.#waitForTest();
        if (this.#disposed || generation !== this.#generation) return;
      }
      const permission = await this.platform.permission(false);
      if (this.#disposed || generation !== this.#generation) return;
      runInAction(() => {
        this.permission = permission;
      });
      if (permission !== 'granted') return;
      const focused = await this.platform.isFocused();
      if (this.#disposed || generation !== this.#generation) return;
      if (focused) {
        runInAction(() => {
          this.notice_message = '当前窗口处于前台，未发送系统通知。可切换窗口后测试延迟通知。';
        });
        return;
      }
      const close = await this.platform.show(
        crypto.randomUUID(),
        'EZ Assistant 测试通知',
        '点击返回应用。',
        () => {
          if (!this.#disposed) void this.platform.focus().catch(() => this.#failure('无法聚焦应用窗口。'));
        },
        () => this.#failure('系统未能显示测试通知。'),
      );
      if (this.#disposed || generation !== this.#generation) {
        close();
        return;
      }
      this.#handles.get('test')?.();
      this.#handles.set('test', close);
      runInAction(() => {
        this.notice_message = '已提交测试通知；是否显示取决于系统通知设置与专注模式。';
      });
      // Notification 构造/原生提交早于 OS 横幅显示；防止连点立即撤回尚未显示的上一条。
      await this.#waitForTest();
    } catch {
      this.#failure('测试通知发送失败，请检查权限；桌面端请使用已安装的应用包。');
    } finally {
      if (!this.#disposed)
        runInAction(() => {
          this.busy = false;
        });
    }
  }

  consumeFocus(target: NotificationTarget): void {
    if (this.focus_target === target) this.focus_target = null;
  }

  dismissNotice(message: string): void {
    if (this.notice_message === message) this.notice_message = null;
  }

  dispose(): void {
    this.#disposed = true;
    this.#generation += 1;
    this.#closeAll();
    window.removeEventListener('focus', this.#onFocus);
    window.removeEventListener('storage', this.#onStorage);
    this.#feedback_navigate = null;
  }

  async #activate(target: NotificationTarget): Promise<void> {
    if (this.#disposed || target.scope !== this.dependencies.scope()) return;
    const activation = ++this.#activation;
    const current = () => !this.#disposed && activation === this.#activation && target.scope === this.dependencies.scope();
    try {
      await this.platform.focus();
      if (!current()) return;
      if (target.kind === 'feedback') {
        if (!this.#feedback_navigate) throw new Error('当前客户端尚不能打开该反馈问题。');
        await this.#feedback_navigate(target);
      } else if ((await this.dependencies.navigate(target)) === false) return;
      if (current())
        runInAction(() => {
          this.focus_target = target;
        });
    } catch {
      if (current()) this.#failure('无法打开通知对应事项，请确认登录状态、连接和会话是否仍可访问。');
    }
  }

  async #waitForTest(): Promise<void> {
    await new Promise<void>((resolve) => {
      const timer = window.setTimeout(resolve, 5000);
      this.#test_delay = { timer, finish: resolve };
    });
    this.#test_delay = null;
  }

  #closeAll(): void {
    if (this.#test_delay) {
      window.clearTimeout(this.#test_delay.timer);
      this.#test_delay.finish();
      this.#test_delay = null;
    }
    for (const close of this.#handles.values()) close();
    this.#handles.clear();
  }

  #failure(message: string): void {
    if (!this.#disposed)
      runInAction(() => {
        this.error_message = message;
      });
  }
}

function trimSet(set: Set<string>): void {
  if (set.size > 1024) {
    const oldest = set.values().next().value;
    if (oldest) set.delete(oldest);
  }
}
