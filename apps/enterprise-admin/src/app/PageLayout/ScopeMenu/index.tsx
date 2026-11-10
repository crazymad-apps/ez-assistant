import { useContext } from 'react';
import { observer } from 'mobx-react-lite';
import { App, Avatar, Button, Dropdown } from 'antd';
import { ApartmentOutlined, DownOutlined } from '@ant-design/icons';
import { useNavigate } from 'react-router';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { api } from '../../../request/api';
import adminStore from '../../../stores/AdminStore';
import { LeaveContext } from '../../NavigationGuard/context';
import { scopeKey, scopeName } from '../../ScopePicker';
import styles from '../header-menu.module.scss';

export const ScopeMenu = observer(function ScopeMenu() {
  const store = adminStore;
  const candidates = useResourceQuery(api.scopes);
  const navigate = useNavigate();
  const leave = useContext(LeaveContext);
  const { message, modal } = App.useApp();
  const scope = store.scope!;
  const current = scopeKey(scope);
  const name = scope.kind === 'system' ? '系统管理' : scope.name;
  const items = candidates.value?.map((option) => ({
    key: scopeKey(option),
    label: option.code ? `${scopeName(option)} · ${option.code}` : scopeName(option),
    disabled: scopeKey(option) === current,
  }));

  async function select(key: string) {
    if (key === 'retry') {
      candidates.reload();
      return;
    }
    const target = candidates.value?.find((option) => scopeKey(option) === key);
    if (!target || key === current || store.busy) return;
    if (leave?.pending) {
      const confirmed = await modal.confirm({
        title: '切换组织？',
        content: '当前页面未保存的修改将被放弃。',
        okText: '继续切换',
        cancelText: '继续编辑',
      });
      if (!confirmed || store.busy) return;
    }
    const problem = await store.exchangeScope({
      kind: target.kind,
      ...(target.organization_id ? { organization_id: target.organization_id } : {}),
    });
    if (problem) {
      void message.error(problem);
      return;
    }
    leave?.discard();
    void navigate(target.kind === 'system' ? '/organizations' : '/members', { replace: true });
  }

  return (
    <Dropdown
      trigger={['click']}
      disabled={store.busy}
      onOpenChange={(open) => {
        if (open && !candidates.loading) candidates.reload();
      }}
      menu={{
        selectedKeys: [current],
        items: candidates.loading
          ? [{ key: 'loading', label: '正在读取组织', disabled: true }]
          : candidates.error
            ? [{ key: 'retry', label: '读取失败，点击重试', title: candidates.error }]
            : items,
        onClick: ({ key }) => void select(key),
      }}
    >
      <Button
        type="text"
        className={styles.menu_button}
        disabled={store.busy}
        title={name}
        aria-label={`切换组织，当前${name}`}
      >
        <Avatar size="small" icon={<ApartmentOutlined />} />
        <span className={styles.menu_label}>{name}</span>
        <DownOutlined />
      </Button>
    </Dropdown>
  );
});
