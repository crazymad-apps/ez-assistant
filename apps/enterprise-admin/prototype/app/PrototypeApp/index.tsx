import { useRef, useState } from 'react';
import { App, Avatar, Breadcrumb, Button, Dropdown, Layout, Menu, Select, Tag } from 'antd';
import { AuditOutlined, DownOutlined, IdcardOutlined, LockOutlined, LogoutOutlined, ReloadOutlined, TeamOutlined, UserOutlined } from '@ant-design/icons';
import { PasswordDialog } from '../../features/account/PasswordDialog';
import { ProfileDialog } from '../../features/account/ProfileDialog';
import { AuditPage } from '../../features/audit/AuditPage';
import { LoginPage } from '../../features/login/LoginPage';
import { ResetPasswordDialog } from '../../features/passwords/ResetPasswordDialog';
import { UserEditor } from '../../features/users/UserEditor';
import { UsersPage } from '../../features/users/UsersPage';
import { createDemoAudit, createDemoUsers, DEMO_PASSWORD, userChangeError } from '../../model';
import type { DemoAudit, DemoUser, Page, PreviewState, UserDraft } from '../../model';
import styles from './index.module.scss';

const PAGE_LABELS: Record<Page, string> = { users: '用户管理', audit: '管理审计' };

/** 仅用于视觉评审的内存装配，不能作为正式身份服务或生产数据源复用。 */
export function PrototypeApp() {
  const { message, modal } = App.useApp();
  const [users, setUsers] = useState(createDemoUsers);
  const [audit, setAudit] = useState(createDemoAudit);
  const [currentId, setCurrentId] = useState('demo-user-0');
  const [page, setPage] = useState<Page>('users');
  const [editor, setEditor] = useState<DemoUser | 'new'>();
  const [passwordTarget, setPasswordTarget] = useState<DemoUser>();
  const [preview, setPreview] = useState<PreviewState>('normal');
  const [notice, setNotice] = useState('');
  // 右上角用户菜单打开的弹窗：账号信息或修改密码。
  const [accountDialog, setAccountDialog] = useState<'profile' | 'password'>();
  // 演示密码只保存在当前组件内存；不落盘、不发送网络、不进入审计。
  const passwords = useRef<Record<string, string>>({});
  const currentUser = users.find(user => user.id === currentId);

  function record(action: string, target: string, detail: string, actor = currentUser?.username ?? '未认证') {
    const entry: DemoAudit = { id: crypto.randomUUID(), time: new Date().toLocaleString('sv-SE'), actor, action, target, success: true, detail };
    setAudit(previous => [entry, ...previous]);
  }
  function leave(reason = '') { setNotice(reason); setCurrentId(''); setEditor(undefined); setPasswordTarget(undefined); }
  function reset() {
    passwords.current = {}; setUsers(createDemoUsers()); setAudit(createDemoAudit()); setCurrentId('demo-user-0');
    setPage('users'); setEditor(undefined); setPasswordTarget(undefined); setPreview('normal'); setNotice(''); setAccountDialog(undefined);
  }
  function navigate(next: Page) {
    const change = () => { setEditor(undefined); setPage(next); };
    if (editor) modal.confirm({ title: '离开用户编辑？', content: '尚未保存的修改将被放弃。', okText: '离开', cancelText: '继续编辑', onOk: change });
    else change();
  }
  function saveUser(draft: UserDraft, original?: DemoUser): string | undefined {
    const username = draft.username.trim().toLowerCase();
    if (users.some(user => user.username === username && user.id !== original?.id)) return '登录账号已存在，请更换账号。';
    if (original) {
      const next = { ...original, displayName: draft.displayName.trim(), role: draft.role };
      const error = userChangeError(users, original, next); if (error) return error;
      setUsers(previous => previous.map(user => user.id === original.id ? next : user));
      record(next.role !== original.role ? '修改角色' : '编辑用户', username, '用户资料已更新（前端模拟）。');
      if (next.id === currentId && next.role !== original.role) leave('角色已修改，请重新登录。');
    } else {
      const user: DemoUser = { id: crypto.randomUUID(), username, displayName: draft.displayName.trim(), role: draft.role, enabled: true, superAdmin: false, createdAt: new Date().toLocaleString('sv-SE') };
      passwords.current[user.id] = draft.password ?? DEMO_PASSWORD;
      setUsers(previous => [user, ...previous]); record('创建用户', username, '新建用户，默认启用（前端模拟）。');
    }
    setEditor(undefined); void message.success(original ? '修改已保存（模拟）' : '用户已创建（模拟）');
    return undefined;
  }
  function toggle(user: DemoUser) {
    const next = { ...user, enabled: !user.enabled };
    const error = userChangeError(users, user, next); if (error) { void message.info(error); return; }
    setUsers(previous => previous.map(item => item.id === user.id ? next : item));
    record(next.enabled ? '启用用户' : '停用用户', user.username, next.enabled ? '允许重新登录，不恢复旧会话。' : '目标用户的登录已模拟撤销。');
    if (user.id === currentId) leave('账号状态已变更，请重新登录。');
    else void message.success(next.enabled ? '用户已启用（模拟）' : '用户已停用（模拟）');
  }

  if (!currentUser) return <LoginPage notice={notice} onReset={reset} onLogin={(username, password) => {
    const user = users.find(item => item.username === username.trim().toLowerCase());
    if (!user?.enabled || password !== (passwords.current[user.id] ?? DEMO_PASSWORD)) return '账号或密码错误，或账号不可用。';
    if (user.role !== 'admin' && !user.superAdmin) return '此账号无管理后台权限，请通过 Runtime 登录。';
    setCurrentId(user.id); setPage('users'); setNotice(''); record('管理员登录', user.username, '通过原型登录页进入。', user.username); return undefined;
  }} />;

  let content;
  if (page === 'audit') content = <AuditPage records={audit} preview={preview} onRecover={() => setPreview('normal')} />;
  else content = <UsersPage users={users} preview={preview} onRecover={() => setPreview('normal')}
    onCreate={() => setEditor('new')} onEdit={setEditor} onResetPassword={setPasswordTarget} onToggle={toggle} />;

  return <Layout className={styles.shell}>
    <Layout.Header className={styles.header}>
      <div className={styles.brand}>
        <span className={styles.brand_icon}>EZ</span>
        <strong>ez-assistant 企业中心</strong>
        <Tag>原型</Tag>
      </div>
      <div className={styles.header_actions}>
        <Select aria-label="预览状态" size="small" value={preview} onChange={setPreview} className={styles.preview_select}
          options={[{ value: 'normal', label: '正常状态' }, { value: 'empty', label: '空数据状态' }, { value: 'failure', label: '加载失败' }]} />
        <Button size="small" type="text" className={styles.header_button} icon={<ReloadOutlined />} onClick={reset}>重置演示</Button>
        {/* 右上角用户菜单：账号信息与修改密码均为弹窗。 */}
        <Dropdown trigger={['click']} menu={{ items: [
          { key: 'profile', icon: <IdcardOutlined />, label: '账号信息' },
          { key: 'password', icon: <LockOutlined />, label: '修改密码' },
          { type: 'divider' },
          { key: 'logout', icon: <LogoutOutlined />, label: '退出登录' },
        ], onClick: ({ key }) => {
          if (key === 'profile') setAccountDialog('profile');
          else if (key === 'password') setAccountDialog('password');
          else { record('退出登录', currentUser.username, '仅退出当前演示登录。'); leave(); }
        } }}>
          <Button type="text" className={styles.account_button}>
            <Avatar size="small" icon={<UserOutlined />} />{currentUser.displayName}<DownOutlined />
          </Button>
        </Dropdown>
      </div>
    </Layout.Header>
    <Layout>
      <Layout.Sider width={200} className={styles.sider}>
        <Menu aria-label="主导航" selectedKeys={[page]} onClick={({ key }) => navigate(key as Page)} items={[
          { key: 'users', icon: <TeamOutlined />, label: '用户管理' },
          { key: 'audit', icon: <AuditOutlined />, label: '管理审计' },
        ]} />
        <div className={styles.sidebar_bottom}>
          <p>交互原型 C01<br />模拟数据 · 刷新后重置</p>
          <Button type="text" size="small" icon={<LogoutOutlined />} onClick={() => leave()}>查看登录页</Button>
        </div>
      </Layout.Sider>
      <Layout.Content className={styles.content}>
        <Breadcrumb className={styles.breadcrumb} items={[{ title: '企业中心' }, { title: PAGE_LABELS[page] }]} />
        {content}
      </Layout.Content>
    </Layout>
    {/* 创建/编辑用户改为弹窗，不占用二级页面。 */}
    {editor && <UserEditor key={editor === 'new' ? 'new' : editor.id} user={editor === 'new' ? undefined : editor} onBack={() => setEditor(undefined)} onSave={saveUser} />}
    {accountDialog === 'profile' && <ProfileDialog user={currentUser} onClose={() => setAccountDialog(undefined)} />}
    {accountDialog === 'password' && <PasswordDialog onClose={() => setAccountDialog(undefined)} onSave={(oldPassword, newPassword) => {
      if (oldPassword !== (passwords.current[currentId] ?? DEMO_PASSWORD)) return '原密码不正确，请重新输入。';
      passwords.current[currentId] = newPassword; record('修改本人密码', currentUser.username, '校验原密码后修改，不记录密码内容。'); leave('密码已修改，请使用新密码重新登录。'); return undefined;
    }} />}
    {passwordTarget && <ResetPasswordDialog user={passwordTarget} currentId={currentId} onClose={() => setPasswordTarget(undefined)} onSave={(user, password) => {
      passwords.current[user.id] = password; record('管理员重置密码', user.username, '管理操作，无需原密码；目标登录模拟失效，不记录密码。');
      setPasswordTarget(undefined);
      if (user.id === currentId) leave('你的密码已重置，请使用新密码重新登录。');
      else void message.success(`已重置 ${user.username} 的密码（模拟）`);
    }} />}
  </Layout>;
}
