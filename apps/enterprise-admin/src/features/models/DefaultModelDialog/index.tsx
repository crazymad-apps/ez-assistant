import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, Button, Modal, Select, Space } from 'antd';
import { observer } from 'mobx-react-lite';
import type { ProviderSummary } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';
import { modelApi } from '../../../request/models';
import { modelPath, modelRows } from '../model';
import { useResourceQuery } from '../../../request/useResourceQuery';

export const DefaultModelDialog = observer(function DefaultModelDialog(props: {
  readonly providers: ProviderSummary[];
  readonly onClose: () => void;
  readonly onSaved: () => void;
}) {
  const [provider, setProvider] = useState<string>(),
    [model, setModel] = useState<string>(),
    [error, setError] = useState('');
  const navigate = useNavigate();
  const read = useCallback(
    async (token: string, signal: AbortSignal) => {
      if (!provider) return [];
      const [catalog, fixed] = await Promise.all([
        modelApi.catalog(token, provider, signal),
        modelApi.fixed(token, provider, signal),
      ]);
      return modelRows(catalog, fixed);
    },
    [provider],
  );
  const query = useResourceQuery(read),
    selected = query.value?.find((item) => item.id === model);
  return (
    <Modal
      open
      title="选择默认模型"
      onCancel={props.onClose}
      confirmLoading={adminStore.busy}
      cancelButtonProps={{ disabled: adminStore.busy }}
      closable={!adminStore.busy}
      okButtonProps={{ disabled: !provider || !selected || selected.requires || query.loading || Boolean(query.error) }}
      onOk={async () => {
        if (!provider || !model) return;
        const failure = await adminStore.setDefaultModel({ provider_instance_id: provider, model_id: model });
        if (failure) setError(failure);
        else props.onSaved();
      }}
    >
      <Space orientation="vertical" style={{ width: '100%' }}>
        {(error || query.error) && <Alert type="error" title={error || query.error} />}
        <Select
          aria-label="服务商"
          placeholder="选择服务商"
          style={{ width: '100%' }}
          value={provider}
          disabled={adminStore.busy}
          options={props.providers.map((item) => ({
            value: item.provider_instance_id,
            label: item.connection.display_name,
          }))}
          onChange={(value) => {
            setProvider(value);
            setModel(undefined);
          }}
        />
        <Select
          aria-label="默认模型"
          placeholder="选择模型"
          style={{ width: '100%' }}
          showSearch
          value={model}
          loading={query.loading}
          disabled={!provider || adminStore.busy || query.loading}
          options={query.value?.map((item) => ({
            value: item.id,
            label: `${item.id} · ${item.origin === 'manual' ? '手动' : '在线'}${item.requires ? ' · 待配置' : ''}`,
          }))}
          onChange={setModel}
        />
        {selected?.requires && provider && model && (
          <Button
            onClick={() => {
              props.onClose();
              void navigate(modelPath(provider, model));
            }}
          >
            完善模型参数
          </Button>
        )}
      </Space>
    </Modal>
  );
});
