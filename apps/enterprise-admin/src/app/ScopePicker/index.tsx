import { useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Alert, Button, Select, Space, Spin, Typography } from 'antd';
import { useResourceQuery } from '../../request/useResourceQuery';
import { api } from '../../request/api';
import type { ScopeView } from '../../request/openapi';
import adminStore from '../../stores/AdminStore';

export function scopeKey(scope: ScopeView) {
  return scope.organization_id ?? scope.kind;
}

export function scopeName(scope: ScopeView) {
  if (scope.kind === 'system') return '系统管理';
  return scope.enabled === false ? `${scope.name}（已停用）` : scope.name;
}

export const SelectInitialScope = observer(function SelectInitialScope() {
  const candidates = useResourceQuery(api.scopes);
  const store = adminStore;
  const [choice, setChoice] = useState<string>();
  const [error, setError] = useState('');
  const target = candidates.value?.find((scope) => scopeKey(scope) === choice) ?? candidates.value?.[0];
  const waiting = store.busy || store.restoreState === 'pending';

  async function submit() {
    if (!target || waiting || store.restoreState === 'failed') return;
    setError('');
    const problem = await store.exchangeScope({
      kind: target.kind,
      ...(target.organization_id ? { organization_id: target.organization_id } : {}),
    });
    if (problem) setError(problem);
  }

  return (
    <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
      <Typography.Text strong>选择组织</Typography.Text>
      {candidates.loading ? (
        <Spin aria-label="正在读取管理范围" />
      ) : candidates.error ? (
        <Alert type="error" title={candidates.error} action={<Button onClick={candidates.reload}>重试</Button>} />
      ) : !candidates.value?.length ? (
        <Alert
          type="info"
          title="当前账号暂无后台管理权限"
          action={<Button onClick={candidates.reload}>重新检查</Button>}
        />
      ) : (
        <>
          {store.restoreState === 'failed' ? (
            <Alert
              type="error"
              title="暂时无法核对登录状态"
              action={<Button onClick={() => void store.verifyIdentity()}>重试</Button>}
            />
          ) : (
            error && <Alert type="error" title={error} />
          )}
          <Select
            aria-label="管理范围"
            placeholder="选择管理范围"
            showSearch={{ optionFilterProp: 'label' }}
            size="large"
            style={{ width: '100%' }}
            value={target && scopeKey(target)}
            onChange={setChoice}
            disabled={waiting || store.restoreState === 'failed'}
            options={candidates.value.map((scope) => ({
              value: scopeKey(scope),
              label: scope.code ? `${scopeName(scope)} · ${scope.code}` : scopeName(scope),
            }))}
          />
          <Button
            type="primary"
            size="large"
            block
            disabled={!target || store.restoreState === 'failed'}
            loading={waiting}
            onClick={() => void submit()}
          >
            进入
          </Button>
        </>
      )}
    </Space>
  );
});
