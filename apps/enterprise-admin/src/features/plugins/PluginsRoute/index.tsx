import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, App, Button, Card, Input, Result, Select, Space, Table, Tag, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { PluginRecord } from '../../../request/openapi';
import { pluginApi } from '../../../request/plugins';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { PluginUploadDialog } from '../PluginUploadDialog';
import { platformLabel, pluginDetailPath, publicationLabel, versionCompare } from '../model';
import styles from './index.module.scss';

export const PluginsRoute = observer(function PluginsRoute() {
  const navigate = useNavigate();
  const { modal, message } = App.useApp();
  const [search, setSearch] = useState('');
  const [filter, setFilter] = useState('');
  const [state, setState] = useState('all');
  const [selectedState, setSelectedState] = useState('all');
  const [uploadOpen, setUploadOpen] = useState(false);
  const [actionError, setActionError] = useState('');
  const read = useCallback((token: string, signal: AbortSignal) => pluginApi.list(token, signal), []);
  const query = useResourceQuery(read);
  const rows = (query.value ?? []).filter(
    (item) =>
      (selectedState === 'all' || item.publication === selectedState) &&
      `${item.name} ${item.display_name} ${item.description}`.toLowerCase().includes(filter.toLowerCase()),
  );

  async function toggle(row: PluginRecord) {
    setActionError('');
    try {
      const current = await pluginApi.get(adminStore.token, row.name);
      if (!current.etag) {
        setActionError('条件写标识缺失，请重新读取。');
        return;
      }
      const listed = current.detail.publication !== 'listed';
      const versions = current.detail.versions
        .map((version) => `${version.version}（${platformLabel(version)}）`)
        .join('；');
      modal.confirm({
        title: `${listed ? '上架' : '下架'}插件「${row.name}」？`,
        content: listed ? (
          `上架后，企业用户可发现并下载匹配的插件包。当前版本：${versions || '无可用完整包'}`
        ) : (
          <Space orientation="vertical">
            <span>下架后停止下载和更新；已有安装仍可继续使用或删除。</span>
            <span>
              中心名称引用：
              {current.detail.dependent_agents.length
                ? current.detail.dependent_agents.join('、')
                : '未发现中心智能体的名称引用'}
            </span>
          </Space>
        ),
        okText: listed ? '上架' : '下架插件',
        okButtonProps: { danger: !listed, disabled: listed && current.detail.versions.length === 0 },
        onOk: async () => {
          const problem = await adminStore.manage(
            (token) => pluginApi.publication(token, row.name, listed, current.etag!),
            () => {
              void message.success(listed ? '插件已上架' : '插件已下架');
              query.reload();
            },
          );
          if (problem) {
            setActionError(problem);
            throw new Error(problem);
          }
        },
      });
    } catch {
      setActionError('插件详情与依赖读取失败，不能根据旧列表执行上下架。');
    }
  }

  if (query.error && !query.value)
    return <Result status="error" title={query.error} extra={<Button onClick={query.reload}>重新读取</Button>} />;
  return (
    <section className={styles.page}>
      <Card
        title="插件"
        className={styles.card}
        extra={
          <Button type="primary" disabled={adminStore.busy} onClick={() => setUploadOpen(true)}>
            上传插件包
          </Button>
        }
      >
        {(query.error || actionError) && (
          <Alert
            type="error"
            title={query.error || actionError}
            action={<Button onClick={query.reload}>重新读取</Button>}
          />
        )}
        <Space wrap className={styles.filters}>
          <Typography.Text>搜索</Typography.Text>
          <Input.Search
            aria-label="搜索插件"
            placeholder="搜索名称、编码或说明"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            onSearch={() => setFilter(search)}
          />
          <Typography.Text>上架状态</Typography.Text>
          <Select
            aria-label="上架状态"
            value={state}
            onChange={setState}
            className={styles.state}
            options={[
              { value: 'all', label: '全部' },
              { value: 'unpublished', label: '未上架' },
              { value: 'listed', label: '在架' },
              { value: 'deprecated', label: '已下架' },
            ]}
          />
          <Button
            onClick={() => {
              setFilter(search);
              setSelectedState(state);
            }}
          >
            查询
          </Button>
          <Button
            onClick={() => {
              setSearch('');
              setFilter('');
              setState('all');
              setSelectedState('all');
            }}
          >
            重置
          </Button>
        </Space>
        <Table<PluginRecord>
          rowKey="name"
          loading={query.loading}
          dataSource={rows}
          pagination={false}
          locale={{ emptyText: query.value?.length ? '没有符合条件的插件' : '暂无插件' }}
          scroll={{ x: 1120 }}
          columns={[
            {
              title: '名称',
              width: 180,
              render: (_, row) => (
                <Button type="link" size="small" onClick={() => navigate(pluginDetailPath(row.name))}>
                  {row.display_name}
                </Button>
              ),
            },
            { title: '编码', dataIndex: 'name', width: 210, ellipsis: { showTitle: true } },
            { title: '说明', dataIndex: 'description', ellipsis: { showTitle: true }, render: (value) => value || '—' },
            {
              title: '上架情况',
              width: 100,
              dataIndex: 'publication',
              render: (value) => (
                <Tag color={value === 'listed' ? 'success' : value === 'deprecated' ? 'warning' : 'default'}>
                  {publicationLabel(value)}
                </Tag>
              ),
            },
            {
              title: '版本概况',
              width: 210,
              render: (_, row) =>
                row.versions.slice().sort((a, b) => versionCompare(a.version, b.version))[0]?.version ?? '暂无完整包',
            },
            {
              title: '操作',
              width: 160,
              fixed: 'right',
              align: 'center',
              render: (_, row) => (
                <Space size={0}>
                  <Button type="link" size="small" onClick={() => navigate(pluginDetailPath(row.name))}>
                    详情
                  </Button>
                  <Button
                    type="link"
                    size="small"
                    danger={row.publication === 'listed'}
                    disabled={adminStore.busy}
                    onClick={() => void toggle(row)}
                  >
                    {row.publication === 'listed' ? '下架' : '上架'}
                  </Button>
                </Space>
              ),
            },
          ]}
        />
      </Card>
      <PluginUploadDialog
        open={uploadOpen}
        onClose={() => setUploadOpen(false)}
        previousShaFor={(name, version) =>
          query.value?.find((item) => item.name === name)?.versions.find((item) => item.version === version)?.sha256
        }
        onUploaded={() => query.reload()}
        onInspect={(name) => {
          if (name) navigate(pluginDetailPath(name));
          else query.reload();
        }}
      />
    </section>
  );
});
