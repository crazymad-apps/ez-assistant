import { observer } from 'mobx-react-lite';
import { createPortal } from 'react-dom';
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '../../../components/DropdownMenu';
import { InlineIconButton } from '../../../components/InlineIconButton';
import { useRootStore } from '../../../stores/RootStoreContext';
import { notificationKey, type NotificationTarget } from '../types';
import styles from './index.module.scss';

export const PendingNotifications = observer(function PendingNotifications() {
  const store = useRootStore();
  const pending = store.pending_notifications;
  const count = pending.items.length;
  const hint = pending.hint;
  if (!pending.scope) return null;

  async function open(target: NotificationTarget) {
    pending.dismissHint();
    const opened = await store.notifications.activate(target);
    if (target.scope !== pending.scope) return;
    if (!opened) {
      store.showInteractionError("无法打开对应事项，请确认连接与会话是否仍可访问。");
    }
    // 点击可能发现事项已处理；重读列表，不把点击本身当作业务完成。
    await pending.refresh();
  }

  return <>
    {(count > 0 || pending.error || pending.syncing) && <DropdownMenu>
      <DropdownMenuTrigger className={styles.trigger}>
        {count > 0 ? `待处理 ${count}${!pending.ready || pending.error ? " · 未同步" : ""}` : pending.syncing ? '正在同步待处理' : '待处理未同步'}
      </DropdownMenuTrigger>
      <DropdownMenuContent aria-label="待处理事项" className={styles.menu}>
        {(!pending.ready || pending.error) && <p className={styles.notice} role="status">待处理事项尚未同步</p>}
        {pending.error && <DropdownMenuItem disabled={pending.syncing || !pending.ready} onSelect={() => void pending.refresh()}>重新同步</DropdownMenuItem>}
        {pending.syncing && <p className={styles.notice} role="status">正在同步…</p>}
        {pending.groups.map((group) => <div aria-label={group.title} key={group.session_id} role="group">
          <p className={styles.title} title={group.title}>{group.title}</p>
          {group.items.map((item) => <DropdownMenuItem
            className={styles.item}
            disabled={!pending.ready}
            key={notificationKey(item.target)}
            onSelect={() => void open(item.target)}
          >{item.label}</DropdownMenuItem>)}
        </div>)}
      </DropdownMenuContent>
    </DropdownMenu>}
    {hint && createPortal(<aside className={styles.hint} role="status">
      <button className={styles.hint_target} onClick={() => void open(hint.target)} type="button">
        <strong>{pending.title(hint.target.owner.session_id)}</strong>
        <span>{hint.label}</span>
      </button>
      <InlineIconButton icon="x" label="关闭提醒" onClick={() => pending.dismissHint()} />
    </aside>, document.querySelector('#overlay-root') ?? document.body)}
  </>;
});
