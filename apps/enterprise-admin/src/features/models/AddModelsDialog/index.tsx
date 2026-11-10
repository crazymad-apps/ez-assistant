import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, Button, Input, Modal, Select, Space, Table } from 'antd';
import type { ModelAvailability, ProviderSummary } from '../../../request/openapi';
import { modelApi } from '../../../request/models';
import { modelPath, modelRows } from '../model';
import { useResourceQuery } from '../../../request/useResourceQuery';

export function AddModelsDialog(props: {
  readonly providers: ProviderSummary[];
  readonly current: ModelAvailability[];
  readonly onClose: () => void;
  readonly onAdd: (items: ModelAvailability[]) => void;
}) {
  const [provider, setProvider] = useState<string>();
  const [selected, setSelected] = useState<React.Key[]>([]);
  const [search, setSearch] = useState('');
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
  const query = useResourceQuery(read);
  const current = new Set(
    props.current.filter((item) => item.provider_instance_id === provider).map((item) => item.model_id),
  );
  const rows = query.value ?? [];

  return (
    <Modal
      open
      title="添加可用模型"
      width={860}
      onCancel={props.onClose}
      okText="添加到列表"
      okButtonProps={{ disabled: !provider || !selected.length || query.loading || Boolean(query.error) }}
      onOk={() => {
        if (!provider) return;
        props.onAdd(
          rows
            .filter((item) => selected.includes(item.id) && !item.requires && !current.has(item.id))
            .map((item) => ({
              provider_instance_id: provider,
              model_id: item.id,
              display_name: item.name,
              origin: item.origin,
              state: 'ready',
              reason: null,
            })),
        );
      }}
    >
      <Space orientation="vertical" style={{ width: '100%' }}>
        <Select
          aria-label="服务商"
          placeholder="选择服务商"
          style={{ width: '100%' }}
          value={provider}
          options={props.providers.map((item) => ({
            value: item.provider_instance_id,
            label: item.connection.display_name,
          }))}
          onChange={(value) => {
            setProvider(value);
            setSelected([]);
            setSearch('');
          }}
        />
        {query.error && (
          <Alert type="error" title={query.error} action={<Button onClick={query.reload}>重试</Button>} />
        )}
        {provider && !query.error && (
          <>
            <Input.Search
              placeholder="搜索名称或模型 ID"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            {rows.length > 0 && rows.every((item) => current.has(item.id)) && (
              <Alert type="info" title="目录中的模型均已添加" />
            )}
            <Table
              rowKey="id"
              size="small"
              loading={query.loading}
              locale={{ emptyText: search ? '没有匹配的模型' : '模型目录为空' }}
              scroll={{ x: 780, y: 280 }}
              pagination={false}
              dataSource={rows.filter((item) =>
                `${item.name ?? ''} ${item.id}`.toLowerCase().includes(search.toLowerCase()),
              )}
              rowSelection={{
                selectedRowKeys: selected,
                preserveSelectedRowKeys: true,
                onChange: setSelected,
                getCheckboxProps: (item) => ({ disabled: item.requires || current.has(item.id) }),
              }}
              columns={[
                {
                  title: '模型名称',
                  render: (_, row) => row.name || '—',
                },
                {
                  title: '模型 ID',
                  dataIndex: 'id',
                },
                {
                  title: '状态',
                  render: (_, row) => {
                    if (current.has(row.id)) return '已添加';
                    if (!row.requires) return row.origin === 'manual' ? '手动' : '在线';
                    return (
                      <Button type="link" onClick={() => navigate(modelPath(provider, row.id))}>
                        待配置 · 完善模型参数
                      </Button>
                    );
                  },
                },
              ]}
            />
          </>
        )}
      </Space>
    </Modal>
  );
}
