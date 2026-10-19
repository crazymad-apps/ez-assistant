import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, App, Button, Card, Form, Input, Modal, Space, Spin, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { HostDeviceDetail, HostDeviceState } from '../../../request/openapi';
import { devicesApi } from '../../../request/devices';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { editable, reducedUsers, sameState, sectionChange, statusLabels } from '../model';
import { UserGrants } from '../UserGrants';
import { DeviceInfo } from '../DeviceInfo';
import { useDeviceRefresh } from '../useDeviceRefresh';

export const DeviceEditor = observer(function DeviceEditor(props: {
  id: string;
  section?: 'info' | 'users';
  disabled?: boolean;
  onActiveChange?: (active: boolean) => void;
  reject?: boolean;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { modal } = App.useApp();
  const [base, setBase] = useState<HostDeviceDetail>();
  const [draft, setDraft] = useState<HostDeviceState>();
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const [blocked, setBlocked] = useState(false);
  const [failedSnapshot, setFailedSnapshot] = useState<HostDeviceDetail>();
  const read = useCallback(
    async (token: string, signal: AbortSignal) => devicesApi.get(token, props.id, signal),
    [props.id],
  );
  const result = useResourceQuery(read);
  const dirty = !!base && !!draft && !sameState(editable(base), draft);
  const inline = !!props.section;
  const lifetime = useRef<AbortController | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    lifetime.current = controller;
    return () => controller.abort();
  }, []);
  useDeviceRefresh(result.reload, !inline || result.loading || dirty || blocked || adminStore.busy);
  useEffect(() => {
    props.onActiveChange?.(dirty || blocked);
    return () => props.onActiveChange?.(false);
  }, [dirty, blocked, props.onActiveChange]);
  // 内联无草稿时跟随最新投影；编辑中或结果未知时必须保留原状态，不能覆盖草稿。
  useEffect(() => {
    if (result.value && (!base || (inline && !dirty && !blocked && base !== result.value))) {
      setBase(result.value);
      const next = editable(result.value);
      if (!inline) next.status = props.reject ? 'rejected' : 'registered';
      setDraft(next);
    }
  }, [base, result.value, props.reject, inline, dirty, blocked]);
  const review =
    blocked && !result.loading && !result.error && result.value !== failedSnapshot ? result.value : undefined;
  useLeaveConfirmation(dirty, props.onClose);
  function close() {
    if (adminStore.busy) return;
    if (!dirty) props.onClose();
    else
      modal.confirm({
        title: inline ? `放弃未保存的${sectionLabel}修改？` : '放弃未保存的设备修改？',
        okText: '放弃修改',
        cancelText: '继续编辑',
        onOk: props.onClose,
      });
  }
  async function save() {
    if (
      !base ||
      !draft ||
      blocked ||
      props.disabled ||
      adminStore.busy ||
      !dirty ||
      !['pending', 'registered'].includes(base.status) ||
      (props.reject && base.status !== 'pending')
    )
      return;
    setError('');
    setSaving(true);
    const problem = await adminStore.manage(
      async (token) => {
        let change = { expected: editable(base), desired: { ...draft, name: draft.name.trim() } };
        if (props.section) {
          const signal = lifetime.current?.signal;
          if (!signal) throw new Error('编辑器已关闭');
          const current = await devicesApi.get(token, props.id, signal);
          signal.throwIfAborted();
          if (adminStore.token !== token) throw new Error('管理身份已变化');
          change = sectionChange(editable(base), draft, editable(current), props.section);
        }
        return devicesApi.save(token, props.id, change);
      },
      () => {
        props.onSaved();
        props.onClose();
      },
    );
    setSaving(false);
    if (problem) {
      setError(problem);
      setBlocked(true);
      setFailedSnapshot(result.value);
    }
  }
  const reduced = base && draft ? reducedUsers(base.users, draft.users) : 0;
  const title = props.reject ? '拒绝申请' : base?.status === 'pending' ? '确认登记' : '分配用户';
  const nameEditor = draft && (
    <Form.Item label="设备名称" required>
      <Input
        aria-label="设备名称"
        placeholder="设备名称"
        value={draft.name}
        disabled={adminStore.busy || blocked || !!props.disabled}
        onChange={(event) => setDraft({ ...draft, name: event.target.value })}
      />
    </Form.Item>
  );
  const userEditor = base && draft && (
    <>
      <Form.Item label={inline ? undefined : '可用用户'}>
        <UserGrants
          users={draft.users}
          identities={base.assigned_users}
          disabled={adminStore.busy || blocked || !!props.disabled}
          onChange={(users) => setDraft({ ...draft, users })}
        />
      </Form.Item>
      {reduced > 0 && <Alert type="warning" title={`降低了 ${reduced} 位用户的权限，相关企业组网访问将停止`} />}
    </>
  );
  const sectionLabel = props.section === 'info' ? '设备信息' : '用户权限';
  const content = (
    <>
      {result.error && (
        <Alert type="error" title={result.error} action={<Button onClick={result.reload}>重试</Button>} />
      )}
      {!draft || !base ? (
        <Spin />
      ) : (
        <Form layout="vertical" onFinish={() => void save()}>
          {!inline && (
            <Typography.Paragraph>
              {base.platform} · 申请人 #{base.applicant_user_id}
            </Typography.Paragraph>
          )}
          {!['pending', 'registered'].includes(base.status) && (
            <Alert type="warning" title={statusLabels[base.status]} />
          )}
          {error && <Alert type="error" title={error} />}
          {blocked && (
            <Space orientation="vertical">
              <Alert type="warning" title="处理结果尚未确认或已发生冲突，草稿已保留；请先查询最新状态" />
              <Button disabled={adminStore.busy} onClick={result.reload}>
                查询最新状态
              </Button>
              {review && (
                <>
                  <Typography.Paragraph>
                    中心当前：{review.name} · {statusLabels[review.status]} · {review.users.length} 位可用用户
                  </Typography.Paragraph>
                  <Button
                    onClick={() => {
                      setBase(review);
                      setDraft({
                        ...editable(review),
                        status:
                          review.status === 'pending' ? (props.reject ? 'rejected' : 'registered') : review.status,
                      });
                      setBlocked(false);
                      setError('');
                      setFailedSnapshot(result.value);
                    }}
                  >
                    采用中心当前结果，重新编辑
                  </Button>
                </>
              )}
            </Space>
          )}
          {props.reject ? (
            <Typography.Paragraph>拒绝“{base.name}”的登记申请？</Typography.Paragraph>
          ) : (
            <>
              {props.section !== 'users' && nameEditor}
              {props.section !== 'info' && userEditor}
            </>
          )}
          <Space>
            <Button
              aria-label={inline ? `放弃${sectionLabel}修改` : undefined}
              onClick={close}
              disabled={adminStore.busy}
            >
              {inline ? '放弃修改' : '取消'}
            </Button>
            <Button
              type="primary"
              aria-label={inline ? `保存${sectionLabel}` : undefined}
              danger={props.reject}
              htmlType="submit"
              loading={saving}
              disabled={
                adminStore.busy ||
                props.disabled ||
                blocked ||
                !dirty ||
                !draft.name.trim() ||
                draft.name.trim().length > 80 ||
                !['pending', 'registered'].includes(base.status) ||
                (props.reject && base.status !== 'pending')
              }
            >
              {inline ? '保存' : title}
            </Button>
          </Space>
        </Form>
      )}
    </>
  );
  if (props.section === 'info') {
    if (!base) return <Card title="设备信息">{content}</Card>;
    return <DeviceInfo device={base} nameEditor={content} />;
  }
  if (props.section === 'users') return <Card title="可用用户">{content}</Card>;
  return (
    <Modal
      open
      title={title}
      width={760}
      footer={null}
      onCancel={close}
      closable={!adminStore.busy}
      keyboard={!adminStore.busy}
      maskClosable={false}
    >
      {content}
    </Modal>
  );
});
