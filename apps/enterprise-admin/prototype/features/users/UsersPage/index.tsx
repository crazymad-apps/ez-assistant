import { useState } from 'react';
import { App, Badge, Button, Card, Empty, Input, Select, Space, Table, Tag, Result } from 'antd';
import { PlusOutlined, SearchOutlined } from '@ant-design/icons';
import type { TableProps } from 'antd';
import { useElementHeight } from '../../../components/useElementHeight';
import type { DemoUser, PreviewState, Role } from '../../../model';
import styles from './index.module.scss';

type UsersPageProps = Readonly<{
  users: readonly DemoUser[];
  preview: PreviewState;
  onRecover: () => void;
  onCreate: () => void;
  onEdit: (user: DemoUser) => void;
  onResetPassword: (user: DemoUser) => void;
  onToggle: (user: DemoUser) => void;
}>;

export function UsersPage(props: UsersPageProps) {
  const { modal } = App.useApp();
  const [draft, setDraft] = useState('');
  const [query, setQuery] = useState('');
  const [role, setRole] = useState<Role | 'all'>('all');
  const [status, setStatus] = useState('all');
  const [page, setPage] = useState(1);
  const rows = props.users.filter(user => `${user.username} ${user.displayName}`.toLowerCase().includes(query.toLowerCase())
    && (role === 'all' || user.role === role)
    && (status === 'all' || String(user.enabled) === status));
  const clear = () => { setDraft(''); setQuery(''); setRole('all'); setStatus('all'); setPage(1); };
  // 列表区高度 = 卡片剩余空间；表格内部滚动，表头与分页保持固定。
  const { ref: listRef, height: listHeight } = useElementHeight<HTMLDivElement>();

  function toggle(user: DemoUser) {
    if (user.superAdmin) { props.onToggle(user); return; }
    modal.confirm({
      title: `${user.enabled ? '停用' : '启用'}用户 ${user.username}？`,
      content: user.enabled ? '该用户的全部登录将失效，需要重新启用后才能登录。' : '启用后允许重新登录，不会恢复旧登录态。',
      okText: user.enabled ? '停用用户' : '启用用户', cancelText: '取消',
      okButtonProps: { danger: user.enabled }, onOk: () => props.onToggle(user),
    });
  }

  const columns: TableProps<DemoUser>['columns'] = [
    // 显示名称与登录账号拆为两列，不再合并展示。
    { title: '显示名称', dataIndex: 'displayName', width: 160, render: (name: string) => <strong>{name}</strong> },
    { title: '登录账号', dataIndex: 'username', width: 180 },
    { title: '角色', key: 'role', width: 200, render: (_, user) => <Space size={8} wrap>
      <Tag color={user.role === 'admin' ? 'blue' : undefined}>{user.role === 'admin' ? '管理员' : '普通用户'}</Tag>
      {user.superAdmin && <Tag color="gold">超级管理员</Tag>}
    </Space> },
    { title: '状态', key: 'status', width: 96, render: (_, user) => <Badge status={user.enabled ? 'success' : 'default'} text={user.enabled ? '启用' : '停用'} /> },
    // 操作列内容默认居中。
    { title: '操作', key: 'actions', width: 240, align: 'center', fixed: 'right', render: (_, user) => <Space size={0}>
      <Button type="link" size="small" onClick={() => props.onEdit(user)}>编辑</Button>
      <Button type="link" size="small" onClick={() => props.onResetPassword(user)}>重置密码</Button>
      <Button type="link" size="small" danger={user.enabled} onClick={() => toggle(user)}>{user.enabled ? '停用' : '启用'}</Button>
    </Space> },
  ];

  return <section className={styles.page} aria-label="用户管理">
    <Card variant="borderless" className={styles.list_card} title={<>成员列表<span className={styles.count}>{rows.length} 条</span></>} extra={
      <Button type="primary" icon={<PlusOutlined />} onClick={props.onCreate}>创建用户</Button>
    }>
      {/* 筛选行：label 与输入控件同行排列。 */}
      <div className={styles.filters}>
        <Input className={styles.search} aria-label="搜索用户" placeholder="搜索账号或显示名称" prefix={<SearchOutlined />} value={draft}
          onChange={event => setDraft(event.target.value)} onPressEnter={() => { setQuery(draft.trim()); setPage(1); }} allowClear />
        <Space size={8}><span className={styles.filter_label}>角色</span><Select aria-label="角色筛选" value={role} className={styles.select} onChange={value => { setRole(value); setPage(1); }}
          options={[{ value: 'all', label: '全部' }, { value: 'admin', label: '管理员' }, { value: 'user', label: '普通用户' }]} /></Space>
        <Space size={8}><span className={styles.filter_label}>状态</span><Select aria-label="状态筛选" value={status} className={styles.select} onChange={value => { setStatus(value); setPage(1); }}
          options={[{ value: 'all', label: '全部' }, { value: 'true', label: '启用' }, { value: 'false', label: '停用' }]} /></Space>
        <Button type="primary" onClick={() => { setQuery(draft.trim()); setPage(1); }}>查询</Button>
        {/* 重置常驻展示，与查询成对出现。 */}
        <Button onClick={clear}>重置</Button>
      </div>
      {props.preview === 'failure' ? <Result status="warning" title="暂时无法加载用户列表" subTitle="这是模拟的网络异常，你的筛选条件仍然保留。" extra={<Button onClick={props.onRecover}>重试</Button>} />
        : <div ref={listRef} className={styles.list_body}>
          <Table<DemoUser> rowKey="id" columns={columns} dataSource={props.preview === 'empty' ? [] : rows}
            scroll={{ x: 880, y: Math.max(listHeight - 100, 160) }}
            rowClassName={user => user.enabled ? '' : (styles.disabled_row ?? '')}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有符合条件的用户" /> }}
            pagination={{ current: page, pageSize: 8, onChange: setPage, showSizeChanger: false, showTotal: total => `共 ${total} 条`, hideOnSinglePage: false }} />
        </div>}
    </Card>
  </section>;
}
