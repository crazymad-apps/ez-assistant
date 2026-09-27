import type { ConversationOwner, RuntimeEventEnvelope } from '@ez-assistant/protocol';

/** 仅定位既有业务事实；F04 在问题可靠发布后提供 request_id，恢复快照不调用 publish。 */
export type NotificationTarget = Readonly<{
  scope: string;
  owner: ConversationOwner;
  run_id: string;
}> &
  (
    | Readonly<{ kind: 'turn' }>
    | Readonly<{ kind: 'approval'; approval_id: string }>
    | Readonly<{ kind: 'feedback'; request_id: string }>
  );

export type SystemNotification = Readonly<{
  target: NotificationTarget;
  title: string;
  body: string;
}>;

export type NotificationPermission = 'default' | 'granted' | 'denied' | 'unsupported' | 'insecure' | 'unavailable';

export interface NotificationPlatform {
  readonly desktop: boolean;
  permission(request: boolean): Promise<NotificationPermission>;
  show(id: string, title: string, body: string, click: () => void, fail: () => void): Promise<() => void>;
  focus(): Promise<void>;
}

export function notificationKey(target: NotificationTarget): string {
  let id = target.run_id;
  if (target.kind === 'approval') id = target.approval_id;
  if (target.kind === 'feedback') id = target.request_id;
  return JSON.stringify([
    target.scope,
    target.owner.session_id,
    target.owner.type === 'child_task' ? target.owner.child_task_id : null,
    target.kind,
    id,
  ]);
}

export function notificationFromEvent(scope: string, envelope: RuntimeEventEnvelope): SystemNotification | null {
  const event = envelope.event;
  if (event.type === 'approval_requested') {
    const approval = event.approval;
    const owner: ConversationOwner = approval.child_task_id
      ? { type: 'child_task', session_id: approval.session_id, child_task_id: approval.child_task_id }
      : { type: 'main_session', session_id: approval.session_id };
    return {
      target: { scope, owner, run_id: approval.run_id, kind: 'approval', approval_id: approval.approval_id },
      title: '需要你的审批',
      body: '点击返回会话查看并处理。',
    };
  }
  if (event.type === 'run_finished' && event.status !== 'cancelled') {
    return {
      target: {
        scope,
        owner: { type: 'main_session', session_id: event.session_id },
        run_id: event.run_id,
        kind: 'turn',
      },
      title: event.status === 'completed' ? '本轮任务已结束' : '任务执行遇到问题',
      body: '点击返回会话查看结果。',
    };
  }
  return null;
}
