import { useCallback, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Alert, App, Button, Card, Space, Spin, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { devicesApi } from '../../../request/devices';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { editable } from '../model';
import { DeviceEditor } from '../DeviceEditor';
import { DeviceInfo } from '../DeviceInfo';
import { useDeviceRefresh } from '../useDeviceRefresh';
import styles from './index.module.scss';

export const DeviceDetailRoute = observer(function DeviceDetailRoute() {
  const { id = '' } = useParams();
  const navigate = useNavigate();
  const { modal } = App.useApp();
  const [editing, setEditing] = useState(false);
  const [infoActive, setInfoActive] = useState(false);
  const [usersActive, setUsersActive] = useState(false);
  const draftActive = infoActive || usersActive;
  const [infoKey, setInfoKey] = useState(0);
  const [usersKey, setUsersKey] = useState(0);
  const [editorKey, setEditorKey] = useState(0);
  const [error, setError] = useState('');
  const [failedSnapshot, setFailedSnapshot] = useState<unknown>();
  const read = useCallback((token: string, signal: AbortSignal) => devicesApi.get(token, id, signal), [id]);
  const result = useResourceQuery(read);
  useDeviceRefresh(result.reload, result.loading || editing || draftActive || adminStore.busy || !!failedSnapshot);
  const row = result.value;
  async function change(kind: 'toggle' | 'revoke') {
    if (!row || adminStore.busy || draftActive || failedSnapshot) return;
    const expected = editable(row);
    const desired =
      kind === 'toggle'
        ? { ...expected, enabled: !row.enabled }
        : { ...expected, status: 'revoked' as const, users: [] };
    const problem = await adminStore.manage(
      (token) => devicesApi.save(token, id, { expected, desired }),
      () => result.reload(),
    );
    if (problem) {
      setError(problem);
      setFailedSnapshot(row);
    }
  }
  function confirm(kind: 'toggle' | 'revoke') {
    if (!row) return;
    if (kind === 'toggle' && !row.enabled) {
      void change(kind);
      return;
    }
    modal.confirm({
      title: kind === 'revoke' ? `注销“${row.name}”？` : `停用“${row.name}”？`,
      content:
        kind === 'revoke'
          ? '注销后需要重新申请登记；本机文件和会话保留，旧设备引用不会自动指向新登记。'
          : '停用后，用户将无法通过企业组网访问该设备，已有组网访问将停止。原有 Host 直连登录保持原规则。',
      okText: kind === 'revoke' ? '注销设备' : '停用设备',
      cancelText: '取消',
      okButtonProps: { danger: true },
      onOk: () => change(kind),
    });
  }
  return (
    <div className={styles.page}>
      <Card>
        <Space wrap>
          <Button disabled={adminStore.busy} onClick={() => void navigate('/devices')}>
            返回设备管理
          </Button>
          <Button
            disabled={result.loading || adminStore.busy || draftActive}
            onClick={() => {
              result.reload();
              setEditorKey((value) => value + 1);
            }}
          >
            刷新详情
          </Button>
        </Space>
      </Card>
      {(result.error || error) && <Alert type="error" title={result.error || error} />}
      {!!failedSnapshot && (
        <Alert
          type="warning"
          title="请刷新详情核对结果后再操作"
          action={
            <Button
              disabled={result.loading || !!result.error || !result.value || result.value === failedSnapshot}
              onClick={() => {
                setFailedSnapshot(undefined);
                setError('');
              }}
            >
              已核对
            </Button>
          }
        />
      )}
      {!row && result.loading && <Spin />}
      {row && (
        <>
          {row.status === 'registered' || draftActive ? (
            <>
              <DeviceEditor
                key={`info:${id}:${editorKey}:${infoKey}`}
                id={id}
                section="info"
                disabled={!!failedSnapshot}
                onActiveChange={setInfoActive}
                onClose={() => setInfoKey((value) => value + 1)}
                onSaved={result.reload}
              />
              <DeviceEditor
                key={`users:${id}:${editorKey}:${usersKey}`}
                id={id}
                section="users"
                disabled={!!failedSnapshot}
                onActiveChange={setUsersActive}
                onClose={() => setUsersKey((value) => value + 1)}
                onSaved={result.reload}
              />
            </>
          ) : (
            <>
              <DeviceInfo device={row} />
              <Card
                title="可用用户"
                extra={
                  row.status === 'pending' && (
                    <Button
                      disabled={adminStore.busy || draftActive || !!failedSnapshot}
                      onClick={() => setEditing(true)}
                    >
                      确认登记
                    </Button>
                  )
                }
              >
                {!row.users.length && <Typography.Text>尚未分配用户</Typography.Text>}
                {row.users.map((grant) => {
                  const user = row.assigned_users.find((user) => user.id === grant.user_id);
                  return (
                    <Typography.Paragraph key={grant.user_id}>
                      {user
                        ? `${user.display_name} / ${user.username}${user.enabled ? '' : '（已停用）'}`
                        : `用户 #${grant.user_id}`}{' '}
                      · 普通业务{grant.desktop_view && ' · 查看桌面'}
                      {grant.desktop_control && ' · 控制桌面'}
                    </Typography.Paragraph>
                  );
                })}
              </Card>
            </>
          )}
          {row.status === 'registered' && (
            <Card title="设备操作">
              <Space wrap>
                <Button
                  danger={row.enabled}
                  disabled={adminStore.busy || draftActive || !!failedSnapshot}
                  onClick={() => confirm('toggle')}
                >
                  {row.enabled ? '停用设备' : '启用设备'}
                </Button>
                <Button
                  danger
                  disabled={adminStore.busy || draftActive || !!failedSnapshot}
                  onClick={() => confirm('revoke')}
                >
                  注销设备
                </Button>
              </Space>
            </Card>
          )}
        </>
      )}
      {editing && <DeviceEditor id={id} onClose={() => setEditing(false)} onSaved={result.reload} />}
    </div>
  );
});
