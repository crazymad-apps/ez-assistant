import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, App, Button, Card, Input, Result, Space, Table, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { AgentSummary } from '../../../request/openapi';
import { agentApi } from '../../../request/agents';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { agentEditPath, dependencyLabel } from '../model';
import styles from './index.module.scss';

export const AgentsRoute = observer(function AgentsRoute() {
  const navigate = useNavigate();
  const { modal, message } = App.useApp();
  const [search, setSearch] = useState('');
  const [filter, setFilter] = useState('');
  const [actionError, setActionError] = useState('');
  const read = useCallback((token: string, signal: AbortSignal) => agentApi.list(token, signal), []);
  const query = useResourceQuery(read);
  const rows = (query.value ?? []).filter((item) =>
    `${item.name} ${item.description}`.toLowerCase().includes(filter.toLowerCase()),
  );

  async function remove(row: AgentSummary) {
    setActionError('');
    try {
      const current = await agentApi.get(adminStore.token, row.name);
      if (!current.etag) {
        setActionError('条件写标识缺失，请重新读取。');
        return;
      }
      modal.confirm({
        title: `删除智能体「${row.name}」？`,
        content: '删除后，企业中心不再提供此智能体供新会话选择。已有会话及已安装插件不受影响。',
        okText: '删除智能体',
        okButtonProps: { danger: true },
        cancelText: '取消',
        onOk: async () => {
          const problem = await adminStore.manage(
            (token) => agentApi.remove(token, row.name, current.etag!),
            () => {
              void message.success('智能体已删除');
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
      setActionError('删除前的最新智能体配置读取失败，请重试。');
    }
  }

  if (query.error && !query.value)
    return <Result status="error" title={query.error} extra={<Button onClick={query.reload}>重新读取</Button>} />;
  return (
    <section className={styles.page}>
      <Card
        title="智能体"
        className={styles.card}
        extra={
          <Button type="primary" disabled={adminStore.busy} onClick={() => navigate('/agents/new')}>
            新建智能体
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
            aria-label="搜索智能体"
            placeholder="搜索名称或描述"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            onSearch={() => setFilter(search)}
          />
          <Button
            onClick={() => {
              setSearch('');
              setFilter('');
            }}
          >
            重置
          </Button>
        </Space>
        <Table<AgentSummary>
          rowKey="name"
          loading={query.loading}
          dataSource={rows}
          pagination={false}
          locale={{ emptyText: query.value?.length ? '没有符合条件的智能体' : '暂无智能体' }}
          scroll={{ x: 900 }}
          columns={[
            {
              title: '名称',
              dataIndex: 'name',
              width: 180,
              render: (_, row) => (
                <Button type="link" size="small" onClick={() => navigate(agentEditPath(row.name))}>
                  {row.name}
                </Button>
              ),
            },
            { title: '描述', dataIndex: 'description', ellipsis: { showTitle: true }, render: (value) => value || '—' },
            {
              title: '默认能力',
              width: 150,
              render: (_, row) =>
                row.skill_count || row.mcp_count ? `Skill ${row.skill_count} · MCP ${row.mcp_count}` : '未设置',
            },
            {
              title: '依赖提示',
              width: 260,
              render: (_, row) => {
                const issues = row.diagnostics.filter((item) => item.status !== 'available');
                return issues.length ? (
                  <Typography.Text type="warning">{issues.map(dependencyLabel).join('；')}</Typography.Text>
                ) : (
                  '—'
                );
              },
            },
            {
              title: '操作',
              width: 150,
              fixed: 'right',
              align: 'center',
              render: (_, row) => (
                <Space size={0}>
                  <Button
                    type="link"
                    size="small"
                    disabled={adminStore.busy}
                    onClick={() => navigate(agentEditPath(row.name))}
                  >
                    编辑
                  </Button>
                  <Button type="link" size="small" danger disabled={adminStore.busy} onClick={() => void remove(row)}>
                    删除
                  </Button>
                </Space>
              ),
            },
          ]}
        />
      </Card>
    </section>
  );
});
