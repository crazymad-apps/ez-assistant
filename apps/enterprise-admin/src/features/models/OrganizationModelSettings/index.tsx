import { useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, App, Button, Card, Radio, Space, Switch, Table, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import type { ModelAvailability, ModelPolicy, ModelSettings, ProviderSummary } from '../../../request/openapi';
import { modelApi } from '../../../request/models';
import adminStore from '../../../stores/AdminStore';
import { AddModelsDialog } from '../AddModelsDialog';
import { modelKey, modelPath, reasons } from '../model';

interface PolicyDraft {
  readonly rows: ModelAvailability[];
  readonly defaultKey: string;
  readonly allowSelection: boolean;
  readonly revision: ModelPolicy['revision'];
}

function policyDraft(settings: ModelSettings): PolicyDraft {
  return {
    rows: settings.availability,
    defaultKey: modelKey(settings.policy?.default_model ?? null),
    allowSelection: settings.policy?.allow_member_selection ?? false,
    revision: settings.policy?.revision ?? null,
  };
}

function sameDraft(left: PolicyDraft, right: PolicyDraft) {
  const keys = new Set(left.rows.map(modelKey));
  return (
    left.defaultKey === right.defaultKey &&
    left.allowSelection === right.allowSelection &&
    keys.size === right.rows.length &&
    right.rows.every((row) => keys.has(modelKey(row)))
  );
}

function selection({ provider_instance_id, model_id }: ModelAvailability) {
  return { provider_instance_id, model_id };
}

export const OrganizationModelSettings = observer(function OrganizationModelSettings(props: {
  readonly initial: ModelSettings;
  readonly providers: ProviderSummary[];
}) {
  const { message, modal } = App.useApp();
  const navigate = useNavigate();
  const [saved, setSaved] = useState(() => policyDraft(props.initial));
  const [draft, setDraft] = useState(saved);
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState('');
  const [recovery, setRecovery] = useState<'conflict' | 'unknown'>();
  const [latest, setLatest] = useState<PolicyDraft>();
  const changed = !sameDraft(saved, draft);
  const defaultRow = draft.rows.find((row) => modelKey(row) === draft.defaultKey);
  const busy = adminStore.busy;
  const locked = busy || Boolean(recovery);
  useLeaveConfirmation(changed || Boolean(recovery), discard);

  function discard() {
    const baseline = latest ?? saved;
    setSaved(baseline);
    setDraft(baseline);
    setAdding(false);
    setError('');
    setRecovery(undefined);
    setLatest(undefined);
  }

  function applySaved(settings: ModelSettings) {
    const baseline = policyDraft(settings);
    setSaved(baseline);
    setDraft(baseline);
    setRecovery(undefined);
    setLatest(undefined);
    setError('');
    void message.success('配置已保存');
  }

  async function submit() {
    if (locked || (draft.rows.length && defaultRow?.state !== 'ready')) return;
    const policy: ModelPolicy = {
      default_model: defaultRow ? selection(defaultRow) : null,
      allowed_models: draft.rows.map(selection),
      allow_member_selection: draft.allowSelection,
      revision: draft.revision,
    };
    setError('');
    const failure = await adminStore.manage(
      (token) => modelApi.savePolicy(token, policy),
      applySaved,
      (code) => {
        if (code === 'MODEL_POLICY_CONFLICT') setRecovery('conflict');
        else if (!code || code === 'SERVICE_UNAVAILABLE') setRecovery('unknown');
      },
    );
    if (failure) setError(failure);
  }

  async function readLatest() {
    const failure = await adminStore.manage(
      (token) => modelApi.settings(token),
      (settings) => {
        const current = policyDraft(settings);
        if (recovery === 'unknown' && sameDraft(current, draft)) {
          applySaved(settings);
        } else if (recovery === 'unknown' && current.revision === draft.revision) {
          setRecovery(undefined);
          setSaved(current);
          setError('已核对，服务端配置尚未更新。');
        } else {
          setLatest(current);
          setRecovery('conflict');
          setError('服务端已有其他配置。请读取最新值后重新编辑。');
        }
      },
    );
    if (failure) setError(failure);
  }

  function save() {
    const retained = new Set(draft.rows.map(modelKey));
    const removed = saved.rows.filter((row) => !retained.has(modelKey(row)));
    const impact = [];
    if (saved.rows.length && !draft.rows.length) {
      impact.push('清空后企业模型将不可用，个人模型、服务商配置和历史保留。');
    } else {
      if (removed.length) impact.push(`移除 ${removed.length} 个可用模型，使用这些模型的后续请求将被拒绝。`);
      if (saved.defaultKey !== draft.defaultKey && saved.defaultKey)
        impact.push('跟随默认的会话将在后续执行中使用新默认，显式选择保持原引用。');
    }
    if (saved.allowSelection && !draft.allowSelection)
      impact.push('关闭自主选择后，成员只能调用默认企业模型，个人模型不受影响。');
    if (impact.length) {
      modal.confirm({ title: '保存组织模型配置？', content: impact.join(' '), onOk: submit });
    } else {
      void submit();
    }
  }

  return (
    <Card title="组织配置">
      <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
        <Space wrap>
          <Switch
            aria-label="允许成员自主选择组织模型"
            checked={draft.allowSelection}
            disabled={locked}
            onChange={(allowSelection) => setDraft({ ...draft, allowSelection })}
          />
          <Typography.Text>允许成员自主选择组织模型</Typography.Text>
          <Typography.Text type="secondary">关闭时只提供默认企业模型，个人模型不受影响。</Typography.Text>
        </Space>
        <Space wrap>
          <Typography.Text strong>可用模型</Typography.Text>
          <Button disabled={locked || !props.providers.length} onClick={() => setAdding(true)}>
            添加模型
          </Button>
          {!props.providers.length && <Typography.Text type="secondary">请先添加服务商</Typography.Text>}
        </Space>
        {changed && <Alert type="info" title="有未保存的修改" />}
        {error && (
          <Alert
            type="error"
            title={error}
            action={
              recovery && (
                <Button disabled={busy} onClick={latest ? discard : readLatest}>
                  {latest ? '采用最新配置' : '核对服务端配置'}
                </Button>
              )
            }
          />
        )}
        {draft.rows.length > 0 && defaultRow?.state !== 'ready' && (
          <Alert type="warning" title={defaultRow ? '默认模型已失效，请修复或重新选择' : '请选择默认模型'} />
        )}
        <Radio.Group
          value={draft.defaultKey}
          onChange={(event) => setDraft({ ...draft, defaultKey: event.target.value })}
          style={{ width: '100%' }}
        >
          <Table
            size="small"
            rowKey={modelKey}
            dataSource={draft.rows}
            pagination={false}
            scroll={{ x: 850 }}
            locale={{ emptyText: '尚未配置企业模型' }}
            columns={[
              {
                title: '默认',
                width: 56,
                render: (_, row) => (
                  <Radio
                    aria-label={`默认模型 ${row.model_id}`}
                    value={modelKey(row)}
                    disabled={locked || row.state !== 'ready'}
                  />
                ),
              },
              {
                title: '模型名称',
                render: (_, row) => row.display_name || '—',
              },
              {
                title: '模型 ID',
                dataIndex: 'model_id',
              },
              {
                title: '服务商',
                render: (_, row) =>
                  props.providers.find((provider) => provider.provider_instance_id === row.provider_instance_id)
                    ?.connection.display_name ?? '已删除',
              },
              {
                title: '状态',
                render: (_, row) => {
                  if (row.state !== 'ready') return reasons[row.reason ?? ''] ?? '不可用';
                  if (!draft.allowSelection && modelKey(row) !== draft.defaultKey) return '关闭自主选择后不向成员提供';
                  return '可用';
                },
              },
              {
                title: '操作',
                render: (_, row) => (
                  <Space>
                    <Button
                      type="link"
                      size="small"
                      disabled={locked || row.reason === 'provider_deleted'}
                      onClick={() => navigate(modelPath(row.provider_instance_id, row.model_id))}
                    >
                      配置
                    </Button>
                    <Button
                      type="link"
                      size="small"
                      disabled={locked}
                      onClick={() =>
                        setDraft({
                          ...draft,
                          rows: draft.rows.filter((entry) => modelKey(entry) !== modelKey(row)),
                          defaultKey: draft.defaultKey === modelKey(row) ? '' : draft.defaultKey,
                        })
                      }
                    >
                      移除
                    </Button>
                  </Space>
                ),
              },
            ]}
          />
        </Radio.Group>
        <Space wrap>
          <Button
            type="primary"
            loading={busy}
            disabled={locked || !changed || (draft.rows.length > 0 && defaultRow?.state !== 'ready')}
            onClick={save}
          >
            保存配置
          </Button>
          <Button disabled={locked || !changed} onClick={discard}>
            取消修改
          </Button>
          <Button
            danger
            disabled={locked || !draft.rows.length}
            onClick={() => setDraft({ ...draft, rows: [], defaultKey: '' })}
          >
            清空可用模型
          </Button>
        </Space>
      </Space>
      {adding && (
        <AddModelsDialog
          providers={props.providers}
          current={draft.rows}
          onClose={() => setAdding(false)}
          onAdd={(rows) => {
            setDraft({ ...draft, rows: [...draft.rows, ...rows] });
            setAdding(false);
          }}
        />
      )}
    </Card>
  );
});
