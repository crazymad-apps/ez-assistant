import { useCallback, useEffect, useState } from 'react';
import { useLocation, useNavigate, useParams } from 'react-router';
import { Alert, Button, Card, Form, Input, Select, Space, Switch, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { ProviderConnection, SaveProviderWritable as SaveProvider } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { modelApi } from '../../../request/models';
import { providerLabels, serverHistorySupport } from '../model';
import { useResourceQuery } from '../../../request/useResourceQuery';
import styles from './index.module.scss';

const defaultConnection: ProviderConnection = {
  display_name: '',
  provider_type: 'openai',
  endpoint: '',
  protocol_preference: 'auto',
  models_path: '',
  discovery_format: 'openai',
  responses_transport: 'http',
  server_history_enabled: false,
  server_history_support_override: null,
};

export function ProviderEditor() {
  const location = useLocation();
  return <ProviderEditorForm key={location.pathname + location.search} />;
}

const ProviderEditorForm = observer(function ProviderEditorForm() {
  const id = useParams().id,
    navigate = useNavigate();
  const [form] = Form.useForm<SaveProvider>(),
    [dirty, setDirty] = useState(false),
    [error, setError] = useState('');
  const read = useCallback(
    async (token: string, signal: AbortSignal) => (id ? modelApi.provider(token, id, signal) : null),
    [id],
  );
  const query = useResourceQuery(read);
  useEffect(() => {
    if (query.value && !dirty)
      form.setFieldsValue({ connection: query.value.connection, credential: { mode: 'unchanged' } });
  }, [query.value, dirty, form]);
  useLeaveConfirmation(dirty, () => setDirty(false));
  const type = Form.useWatch(['connection', 'provider_type'], form) ?? 'openai';
  const credentialMode = Form.useWatch(['credential', 'mode'], form);
  const responses = ['openai', 'deepseek', 'dashscope_api', 'dashscope_plan', 'moonshot'].includes(type);
  const watchedConnection = Form.useWatch('connection', form);
  const historySupport = serverHistorySupport({ ...defaultConnection, ...watchedConnection });
  useEffect(() => {
    const current = form.getFieldValue('connection') as ProviderConnection | undefined;
    if (current?.server_history_enabled && serverHistorySupport(current) !== 'supported') {
      form.setFieldValue(['connection', 'server_history_enabled'], false);
      setDirty(true);
    }
  }, [historySupport, form]);

  const back = () => navigate(id ? `/models/providers/${id}` : '/models');

  async function save(input: SaveProvider) {
    if (input.connection.server_history_enabled && serverHistorySupport(input.connection) !== 'supported') {
      setError('当前接口尚未确认支持服务端历史续接，请先调整续接能力。');
      return;
    }
    let discovery_format: ProviderConnection['discovery_format'] = 'openai';
    if (input.connection.provider_type === 'dashscope_api') discovery_format = 'dashscope_native';
    else if (input.connection.provider_type === 'moonshot' || input.connection.provider_type === 'vllm')
      discovery_format = input.connection.provider_type;
    let savedId = id;
    const credential = input.credential.mode === 'replace' ? input.credential : { mode: input.credential.mode };
    const failure = await adminStore.saveModelProvider(
      { connection: { ...input.connection, discovery_format }, credential },
      id,
      (value) => {
        savedId = value;
      },
    );
    if (failure) {
      setError(failure);
      return;
    }
    setDirty(false);
    form.resetFields([['credential', 'value']]);
    // 先释放离开登记；下一次 effect 中导航，避免刚保存仍出现放弃确认。
    setDestination(`/models/providers/${savedId}`);
  }

  const [destination, setDestination] = useState<string>();
  useEffect(() => {
    if (destination && !dirty) void navigate(destination);
  }, [destination, dirty, navigate]);
  return (
    <section className={styles.page}>
      <Card title={id ? '编辑服务商' : '添加服务商'} loading={Boolean(id && !query.value && query.loading)}>
        {(error || query.error) && (
          <Alert
            type="error"
            title={error || query.error}
            action={query.error && <Button onClick={query.reload}>重新读取</Button>}
          />
        )}
        <Form
          form={form}
          layout="vertical"
          initialValues={{ connection: defaultConnection, credential: { mode: id ? 'unchanged' : 'replace' } }}
          disabled={adminStore.busy || Boolean(id && !query.value)}
          onValuesChange={() => setDirty(true)}
          onFinish={save}
        >
          <Form.Item
            name={['connection', 'display_name']}
            label="显示名称"
            rules={[{ required: true, whitespace: true }]}
          >
            <Input placeholder="服务商名称" />
          </Form.Item>
          <Form.Item name={['connection', 'provider_type']} label="服务商类型">
            <Select
              options={Object.entries(providerLabels).map(([value, label]) => ({ value, label }))}
              onChange={() => {
                form.setFieldValue(['connection', 'protocol_preference'], 'auto');
                form.setFieldValue(['connection', 'responses_transport'], 'http');
                form.setFieldValue(['connection', 'server_history_support_override'], null);
              }}
            />
          </Form.Item>
          <Form.Item
            name={['connection', 'endpoint']}
            label="服务地址"
            rules={[{ required: true }, { type: 'url', message: '请输入完整 HTTP 或 HTTPS 地址' }]}
            extra={id ? '迁移到另一不共享模型状态的服务时，请新建服务商。' : undefined}
          >
            <Input placeholder="https://服务商地址/v1" />
          </Form.Item>
          <Form.Item name={['credential', 'mode']} label="API Key">
            <Select
              options={[
                { value: 'unchanged', label: '保持原值' },
                { value: 'replace', label: '替换' },
                { value: 'clear', label: '清除' },
              ]}
            />
          </Form.Item>
          {credentialMode === 'replace' && (
            <Form.Item name={['credential', 'value']} initialValue="">
              <Input.Password
                aria-label="新的 API Key"
                placeholder="输入新的 API Key；无凭据服务可留空"
                autoComplete="new-password"
              />
            </Form.Item>
          )}
          <Form.Item
            name={['connection', 'protocol_preference']}
            label="协议偏好"
            extra={`自动选择：${responses ? 'Responses' : 'Chat Completions'}`}
          >
            <Select
              onChange={(value) => {
                if (value === 'chat_completions') form.setFieldValue(['connection', 'responses_transport'], 'http');
              }}
              options={[
                { value: 'auto', label: '自动' },
                ...(responses ? [{ value: 'responses', label: 'Responses' }] : []),
                { value: 'chat_completions', label: 'Chat Completions' },
              ]}
            />
          </Form.Item>
          <Form.Item
            name={['connection', 'responses_transport']}
            label="Responses 上游传输"
            extra="WebSocket 需要上游入口及对应凭据支持；客户端仍通过 HTTP 调用企业中心。"
          >
            <Select
              disabled={!responses || watchedConnection?.protocol_preference === 'chat_completions'}
              options={[
                { value: 'http', label: 'HTTP' },
                { value: 'websocket', label: 'WebSocket' },
              ]}
            />
          </Form.Item>
          <Form.Item
            name={['connection', 'models_path']}
            label="模型发现路径"
            extra="留空使用默认路径；填写同源绝对路径。"
          >
            <Input placeholder="例如 /v1/models" />
          </Form.Item>
          <Form.Item
            name={['connection', 'server_history_support_override']}
            label="续接能力"
            getValueProps={(value: ProviderConnection['server_history_support_override']) => ({
              value: value ?? 'default',
            })}
            getValueFromEvent={(value: 'default' | 'supported' | 'unsupported') => (value === 'default' ? null : value)}
          >
            <Select
              options={[
                { value: 'default', label: '使用服务商默认' },
                { value: 'supported', label: '明确支持' },
                { value: 'unsupported', label: '明确不支持' },
              ]}
            />
          </Form.Item>
          <Form.Item
            name={['connection', 'server_history_enabled']}
            label="使用服务端历史续接"
            valuePropName="checked"
            extra={
              historySupport === 'supported'
                ? '开启后复用服务端上下文，减少历史上传；连接或历史失效时用本地上下文重建。'
                : '当前接口尚未确认支持续接。自定义接口需先声明支持；Chat Completions 不支持续接。'
            }
          >
            <Switch disabled={historySupport !== 'supported' || adminStore.busy || Boolean(id && !query.value)} />
          </Form.Item>
          <Typography.Paragraph type="secondary">
            发现格式由服务商类型决定。保存连接不会自动刷新在线模型。
          </Typography.Paragraph>
          <Space>
            <Button type="primary" htmlType="submit" loading={adminStore.busy}>
              保存服务商
            </Button>
            <Button onClick={back}>取消</Button>
          </Space>
        </Form>
      </Card>
    </section>
  );
});
