import { useCallback, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Alert, App, Button, Card, Descriptions, Input, Space, Table, Tag, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import adminStore from '../../../stores/AdminStore';
import { modelApi } from '../../../request/models';
import { modelPath, modelRows, providerLabels } from '../model';
import { useResourceQuery } from '../../../request/useResourceQuery';
import styles from './index.module.scss';

export const ProviderRoute = observer(function ProviderRoute() {
  const id = useParams().id!,
    navigate = useNavigate(),
    { message, modal } = App.useApp();
  const [search, setSearch] = useState(''),
    [actionError, setActionError] = useState('');
  const read = useCallback(
    async (token: string, signal: AbortSignal) => {
      const [provider, catalog, fixed, settings] = await Promise.all([
        modelApi.provider(token, id, signal),
        modelApi.catalog(token, id, signal),
        modelApi.fixed(token, id, signal),
        modelApi.settings(token, signal),
      ]);
      return { provider, catalog, rows: modelRows(catalog, fixed), settings };
    },
    [id],
  );
  const query = useResourceQuery(read),
    data = query.value;
  const usageRead = useCallback((token: string, signal: AbortSignal) => modelApi.usage(token, id, signal), [id]);
  const usage = useResourceQuery(usageRead);

  async function refresh() {
    setActionError('');
    const error = await adminStore.refreshModels(id);
    if (error) setActionError(error);
    else {
      query.reload();
      usage.reload();
    }
  }

  function remove() {
    if (!usage.value || usage.loading || usage.error) return;
    modal.confirm({
      title: '删除服务商？',
      content: `将删除 ${usage.value.fixed_config_count} 条固定配置。${usage.value.default_model ? '企业默认引用将失效，需要重新选择。' : ''}历史与默认引用保留。`,
      okButtonProps: { danger: true },

      onOk: async () => {
        const error = await adminStore.deleteModelProvider(id);
        if (error) {
          void message.error(error);
          throw new Error(error);
        }
        void navigate('/models');
      },
    });
  }

  return (
    <section className={styles.page}>
      <Card>
        <Space wrap>
          <Button onClick={() => navigate('/models')}>返回模型管理</Button>
          <Button disabled={adminStore.busy || !data} onClick={() => navigate(`/models/providers/${id}/edit`)}>
            编辑服务商
          </Button>
          <Button
            danger
            disabled={adminStore.busy || usage.loading || Boolean(usage.error) || !usage.value}
            onClick={remove}
          >
            删除服务商
          </Button>
        </Space>
      </Card>
      {(query.error || actionError) && (
        <Alert
          type="error"
          title={query.error || actionError}
          action={<Button onClick={query.reload}>重新读取</Button>}
        />
      )}
      {usage.error && (
        <Alert type="warning" title="删除影响查询失败" action={<Button onClick={usage.reload}>重试</Button>} />
      )}
      <Card title={data?.provider.connection.display_name ?? '服务商'} loading={!data && query.loading}>
        {data && (
          <Descriptions
            size="small"
            column={{ xs: 1, sm: 2 }}
            items={[
              { key: 'type', label: '类型', children: providerLabels[data.provider.connection.provider_type] },
              { key: 'endpoint', label: '服务地址', children: data.provider.connection.endpoint },
              { key: 'protocol', label: '协议偏好', children: data.provider.connection.protocol_preference },
              { key: 'path', label: '模型发现路径', children: data.provider.connection.models_path || '默认路径' },
              { key: 'key', label: '凭据', children: data.provider.has_api_key ? '已设置' : '未设置' },
            ]}
          />
        )}
      </Card>
      <Card title="模型列表" className={styles.models}>
        <Space wrap className={styles.toolbar}>
          <Input.Search
            placeholder="搜索名称或模型 ID"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
          <Button disabled={adminStore.busy} onClick={() => navigate(`/models/providers/${id}/models/new`)}>
            添加模型
          </Button>
          <Button loading={adminStore.busy} onClick={refresh}>
            刷新在线模型
          </Button>
        </Space>
        <Table
          rowKey="id"
          size="small"
          loading={query.loading}
          dataSource={data?.rows.filter((row) =>
            `${row.id} ${row.name ?? ''}`.toLowerCase().includes(search.toLowerCase()),
          )}
          footer={() => (
            <Typography.Text type="secondary">
              {data?.catalog.refreshed_at_ms
                ? `最近成功刷新：${new Date(data.catalog.refreshed_at_ms).toLocaleString()}`
                : '尚未刷新，可显式刷新或手动添加模型'}
              {data?.catalog.connection_changed && ' · 连接已变化，请重新刷新'}
              {data?.catalog.refreshed_at_ms && data.catalog.models.length === 0 && ' · 本次在线列表为空'}
            </Typography.Text>
          )}
          pagination={false}
          scroll={{ x: 750 }}
          columns={[
            {
              title: '模型名称 / ID',

              render: (_, row) => (
                <Space orientation="vertical" size={0}>
                  {row.name && <span>{row.name}</span>}
                  <span>{row.id}</span>
                </Space>
              ),
            },
            {
              title: '来源',

              render: (_, row) => (
                <Space wrap>
                  <Tag>{row.origin === 'manual' ? '手动' : '在线'}</Tag>
                  {!row.online && <Tag>不在当前在线目录</Tag>}
                </Space>
              ),
            },
            {
              title: '配置状态',

              render: (_, row) => (
                <Space wrap>
                  {row.fixed && <Tag>已固定</Tag>}
                  {row.template && <Tag>使用模板</Tag>}
                  {row.requires && <Tag color="warning">待配置</Tag>}
                  {data?.settings.default_model?.provider_instance_id === id &&
                    data.settings.default_model.model_id === row.id && <Tag color="blue">默认</Tag>}
                </Space>
              ),
            },
            {
              title: '操作',
              width: 100,
              fixed: 'right',
              align: 'center',

              render: (_, row) => (
                <Button type="link" size="small" onClick={() => navigate(modelPath(id, row.id))}>
                  参数
                </Button>
              ),
            },
          ]}
        />
      </Card>
    </section>
  );
});
