import { useCallback, useEffect, useState } from 'react';
import { useLocation, useNavigate, useSearchParams } from 'react-router';
import { Alert, App, Button, Card, Form, Input, Result, Space, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { AgentCapabilityReference, AgentConfig } from '../../../request/openapi';
import { agentApi } from '../../../request/agents';
import { pluginApi } from '../../../request/plugins';
import adminStore from '../../../stores/AdminStore';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { AgentCapabilities } from '../AgentCapabilities';
import { agentNameError } from '../model';
import styles from './index.module.scss';

export function AgentEditor() {
  const location = useLocation();
  return <AgentEditorForm key={location.pathname + location.search} />;
}

const AgentEditorForm = observer(function AgentEditorForm() {
  const [params] = useSearchParams();
  const editing = useLocation().pathname === '/agents/edit';
  const name = params.get('name') ?? '';
  const navigate = useNavigate();
  const { modal, message } = App.useApp();
  const [form] = Form.useForm<AgentConfig>();
  const [skills, setSkills] = useState<AgentCapabilityReference[]>([]);
  const [mcp, setMcp] = useState<AgentCapabilityReference[]>([]);
  const [dirty, setDirty] = useState(false);
  const [etag, setEtag] = useState<string>();
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [destination, setDestination] = useState<string>();

  const read = useCallback(
    (token: string, signal: AbortSignal) =>
      editing && name ? agentApi.get(token, name, signal) : Promise.resolve(null),
    [editing, name],
  );
  const query = useResourceQuery(read);
  const readPlugins = useCallback((token: string, signal: AbortSignal) => pluginApi.list(token, signal), []);
  const catalog = useResourceQuery(readPlugins);

  useEffect(() => {
    if (!dirty && query.value?.detail) {
      const detail = query.value.detail;
      form.setFieldsValue({ name: detail.name, description: detail.description, prompt: detail.prompt });
      setSkills(detail.skills);
      setMcp(detail.mcp);
      setEtag(query.value.etag);
    }
  }, [query.value, dirty, form]);
  useLeaveConfirmation(dirty, () => setDirty(false));
  useEffect(() => {
    if (destination && !dirty) void navigate(destination);
  }, [destination, dirty, navigate]);

  async function save(values: AgentConfig) {
    setError('');
    setConflict(false);
    const body: AgentConfig = {
      name: editing ? name : values.name,
      description: values.description ?? '',
      prompt: values.prompt,
      skills,
      mcp,
    };
    if (editing && !etag) {
      setError('缺少当前 ETag，请重新读取后操作。');
      return;
    }
    const problem = await adminStore.manage(
      (token) => (editing ? agentApi.update(token, name, body, etag!) : agentApi.create(token, body)),
      () => {
        setDirty(false);
        setDestination('/agents');
        void message.success('智能体已保存');
      },
      (code) => {
        if (code === 'CONFIG_CHANGED') setConflict(true);
      },
    );
    if (problem) setError(problem);
  }

  async function viewLatest() {
    try {
      const current = await agentApi.get(adminStore.token, name);
      modal.confirm({
        title: '中心当前配置',
        width: 720,
        content: (
          <Space orientation="vertical" className={styles.full}>
            <Typography.Text>当前描述：{current.detail.description || '—'}</Typography.Text>
            <Input.TextArea aria-label="中心当前提示词" value={current.detail.prompt} readOnly rows={12} />
            <Typography.Text type="secondary">
              当前 Skill {current.detail.skills.length} 项 · MCP {current.detail.mcp.length} 项。原草稿尚未覆盖。
            </Typography.Text>
          </Space>
        ),
        okText: '放弃草稿并载入最新',
        cancelText: '保留草稿',
        onOk: () => {
          form.setFieldsValue({
            name: current.detail.name,
            description: current.detail.description,
            prompt: current.detail.prompt,
          });
          setSkills(current.detail.skills);
          setMcp(current.detail.mcp);
          setEtag(current.etag);
          setDirty(false);
          setConflict(false);
          setError('');
          query.reload();
        },
      });
    } catch {
      setError('最新配置读取失败，请稍后再试；当前草稿仍保留。');
    }
  }

  if (editing && !name)
    return (
      <Result
        status="404"
        title="缺少智能体名称"
        extra={<Button onClick={() => navigate('/agents')}>返回智能体管理</Button>}
      />
    );
  if (editing && query.error && !query.value)
    return (
      <Result
        status="error"
        title={query.error}
        extra={
          <Space>
            <Button onClick={query.reload}>重试</Button>
            <Button onClick={() => navigate('/agents')}>返回列表</Button>
          </Space>
        }
      />
    );

  const unavailable = editing && !query.value?.detail;
  return (
    <section className={styles.page}>
      <Card title={editing ? '编辑智能体' : '新建智能体'} loading={Boolean(unavailable && query.loading)}>
        {(error || query.error || catalog.error) && (
          <Alert
            type="error"
            title={error || query.error || catalog.error}
            action={
              conflict ? (
                <Button onClick={() => void viewLatest()}>查看最新内容</Button>
              ) : query.error || catalog.error ? (
                <Button
                  onClick={() => {
                    query.reload();
                    catalog.reload();
                  }}
                >
                  重试读取
                </Button>
              ) : undefined
            }
          />
        )}
        <Form<AgentConfig>
          form={form}
          layout="vertical"
          initialValues={{ name: '', description: '', prompt: '' }}
          disabled={adminStore.busy || Boolean(unavailable)}
          onValuesChange={() => setDirty(true)}
          onFinish={save}
          onFinishFailed={({ errorFields }) => {
            if (errorFields[0]) void form.scrollToField(errorFields[0].name, { focus: true });
          }}
        >
          <Form.Item
            name="name"
            label="名称"
            rules={[
              {
                validator: async (_, value: string) => {
                  const problem = agentNameError(value);
                  if (problem) throw new Error(problem);
                },
              },
            ]}
            extra={editing ? '名称用于引用，创建后不可修改。' : undefined}
          >
            <Input placeholder="例如 文档助手" disabled={editing} />
          </Form.Item>
          <Form.Item
            name="description"
            label="描述"
            rules={[
              {
                validator: async (_, value: string) => {
                  if ([...(value ?? '')].length > 4096) throw new Error('描述不能超过 4096 个字符。');
                },
              },
            ]}
          >
            <Input.TextArea placeholder="简要说明这个智能体适合处理的任务" rows={3} />
          </Form.Item>
          <Form.Item
            name="prompt"
            label="完整提示词"
            rules={[
              {
                validator: async (_, value: string) => {
                  if (!value?.trim()) throw new Error('请填写完整提示词。');
                  if (new TextEncoder().encode(value).byteLength > 256 * 1024)
                    throw new Error('提示词不能超过 256 KiB UTF-8 字节。');
                },
              },
            ]}
            extra="保留原始换行和空格；修改仅影响后续创建的会话。"
          >
            <Input.TextArea
              placeholder="填写完整提示词"
              autoSize={{ minRows: 14, maxRows: 24 }}
              className={styles.prompt}
            />
          </Form.Item>
          <div className={styles.capabilities}>
            <AgentCapabilities
              kind="skills"
              value={skills}
              onChange={(next) => {
                setSkills(next);
                setDirty(true);
              }}
              plugins={catalog.value ?? []}
              diagnostics={query.value?.detail.diagnostics ?? []}
              disabled={adminStore.busy || Boolean(unavailable)}
            />
            <AgentCapabilities
              kind="mcp"
              value={mcp}
              onChange={(next) => {
                setMcp(next);
                setDirty(true);
              }}
              plugins={catalog.value ?? []}
              diagnostics={query.value?.detail.diagnostics ?? []}
              disabled={adminStore.busy || Boolean(unavailable)}
            />
          </div>
          <Space className={styles.actions}>
            <Button type="primary" htmlType="submit" loading={adminStore.busy}>
              保存智能体
            </Button>
            <Button onClick={() => navigate('/agents')}>取消</Button>
          </Space>
        </Form>
      </Card>
    </section>
  );
});
