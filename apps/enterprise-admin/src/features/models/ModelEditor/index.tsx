import { useCallback, useEffect, useState } from 'react';
import { useLocation, useNavigate, useParams, useSearchParams } from 'react-router';
import { Alert, App, Button, Card, Collapse, Descriptions, Form, Input, Space, Tag, Tooltip, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { ModelParameters } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { modelApi } from '../../../request/models';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { ModelTest } from '../ModelTest';
import { ParameterFields } from '../ParameterFields';
import styles from './index.module.scss';

export function ModelEditor() {
  const location = useLocation();
  return <ModelEditorForm key={location.pathname + location.search} />;
}

const ModelEditorForm = observer(function ModelEditorForm() {
  const id = useParams().id!,
    [search] = useSearchParams(),
    navigate = useNavigate(),
    { modal, message } = App.useApp();
  const existingId = search.get('model_id'),
    [model, setModel] = useState(existingId ?? ''),
    [chosen, setChosen] = useState(existingId ?? '');
  const [draft, setDraft] = useState<ModelParameters>(),
    [dirty, setDirty] = useState(false),
    [error, setError] = useState('');
  const [online, setOnline] = useState<ModelParameters | null>(),
    [savedNew, setSavedNew] = useState(false);
  const read = useCallback(
    async (token: string, signal: AbortSignal) =>
      chosen
        ? modelApi.configuration(
            token,
            { provider_instance_id: id, model_id: chosen, origin: existingId ? 'online' : 'manual' },
            signal,
          )
        : null,
    [id, chosen, existingId],
  );
  const query = useResourceQuery(read),
    detail = query.value;
  useEffect(() => {
    if (detail) setDraft((previous) => (dirty && previous ? previous : structuredClone(detail.parameters)));
  }, [detail, dirty]);
  useLeaveConfirmation(dirty, () => setDirty(false));

  const back = () => navigate(`/models/providers/${id}`);

  async function save() {
    if (!draft || !detail) return;
    const failure = await adminStore.saveModel({
      provider_instance_id: id,
      model_id: chosen,
      origin: detail.origin,
      parameters: draft,
    });
    if (failure) setError(failure);
    else {
      setError('');
      setDirty(false);
      setSavedNew(true);
      query.reload();
      void message.success('固定配置已保存');
    }
  }

  async function refreshOnline() {
    const failure = await adminStore.refreshModels(id);
    if (failure) {
      setError(failure);
      return;
    }
    // 只更新参考区，读取完成由身份/卸载守卫保护，不覆盖已有草稿。
    reference.reload();
  }

  const referenceRead = useCallback(
    async (token: string, signal: AbortSignal) =>
      chosen
        ? ((await modelApi.catalog(token, id, signal)).models.find((item) => item.model_id === chosen)?.metadata ??
          null)
        : null,
    [id, chosen],
  );
  const reference = useResourceQuery(referenceRead);
  useEffect(() => {
    if (reference.value !== undefined) setOnline(reference.value);
  }, [reference.value]);

  function reset() {
    if (!detail) return;
    modal.confirm({
      title: detail.origin === 'manual' ? '删除手动模型？' : '重置固定配置？',
      content: '默认引用及历史保留；无可用在线参数时默认模型将不可用。同 ID 在线条目不会从上游删除。',

      onOk: async () => {
        const failure = await adminStore.resetModel({ provider_instance_id: id, model_id: chosen });
        if (failure) {
          setError(failure);
          throw new Error(failure);
        }
        setDirty(false);
        setDraft(undefined);
        setSavedNew(false);
        if (detail.origin === 'manual') setLeaving(true);
        else query.reload();
      },
    });
  }

  const [leaving, setLeaving] = useState(false);
  useEffect(() => {
    if (leaving && !dirty) void navigate(`/models/providers/${id}`);
  }, [leaving, dirty, id, navigate]);
  return (
    <section className={styles.page}>
      <Card
        classNames={{ body: styles.content }}
        title={
          <div className={styles.heading}>
            <span>{existingId ? '模型参数' : '添加模型'}</span>
            {chosen && (
              <Typography.Text type="secondary" className={styles.model_id}>
                模型 ID：{chosen}
              </Typography.Text>
            )}
            {dirty ? (
              <Tag color="warning">未保存修改</Tag>
            ) : (
              detail?.source === 'fixed' && (
                <Tooltip title="调用使用已保存的参数。刷新模型列表或模板不会修改这些参数；编辑后需保存才会生效。">
                  <Tag tabIndex={0}>已保存配置</Tag>
                </Tooltip>
              )
            )}
          </div>
        }
        extra={<Button onClick={back}>返回服务商</Button>}
      >
        {(error || query.error) && (
          <Alert
            type="error"
            title={error || query.error}
            action={query.error && <Button onClick={query.reload}>重新读取</Button>}
          />
        )}
        {!existingId && !chosen && (
          <Space.Compact>
            <Input
              placeholder="输入真实模型 ID"
              value={model}
              onChange={(event) => {
                setModel(event.target.value);
                setDirty(Boolean(event.target.value));
              }}
            />
            <Button disabled={!model.trim()} onClick={() => setChosen(model.trim())}>
              配置参数
            </Button>
          </Space.Compact>
        )}
        {detail && draft && (
          <Form layout="vertical" className={styles.form} onFinish={save}>
            <ParameterFields
              value={draft}
              sources={
                detail.source === 'fixed'
                  ? Object.fromEntries(Object.keys(draft).map((key) => [key, 'fixed']))
                  : detail.field_sources
              }
              disabled={adminStore.busy}
              onChange={(value) => {
                setDraft(value);
                setDirty(true);
              }}
            />
            {detail.template_document && (
              <Descriptions
                size="small"
                column={1}
                items={[
                  {
                    key: 'document',
                    label: '规格参考',
                    children: (
                      <a href={detail.template_document} target="_blank" rel="noreferrer">
                        厂商文档
                      </a>
                    ),
                  },
                  { key: 'date', label: '模板核查日期', children: detail.template_checked_on },
                ]}
              />
            )}
            <Space className={styles.actions} wrap>
              <Button type="primary" htmlType="submit" loading={adminStore.busy}>
                保存固定配置
              </Button>
              {(detail.source === 'fixed' || savedNew) && (
                <Button danger disabled={adminStore.busy} onClick={reset}>
                  {detail.origin === 'manual' ? '删除模型' : '重置配置'}
                </Button>
              )}
              <Button disabled={adminStore.busy} onClick={back}>
                取消
              </Button>
            </Space>
            <Card title="配置测试" size="small" type="inner">
              <ModelTest
                selection={{ provider_instance_id: id, model_id: chosen }}
                disabled={dirty || detail.source !== 'fixed' || detail.requires_configuration}
              />
            </Card>
            <Collapse
              items={[
                {
                  key: 'online',
                  label: '在线参数参考',
                  children: (
                    <Space orientation="vertical" className={styles.reference}>
                      <Button disabled={adminStore.busy} onClick={refreshOnline}>
                        刷新在线参考
                      </Button>
                      {reference.error && <Alert type="error" title={reference.error} />}
                      {online ? (
                        <>
                          <div className={styles.comparison}>
                            <div>
                              <Typography.Text>当前草稿</Typography.Text>
                              <pre>{JSON.stringify(draft, null, 2)}</pre>
                            </div>
                            <div>
                              <Typography.Text>当前在线值（不含模板）</Typography.Text>
                              <pre>{JSON.stringify(online, null, 2)}</pre>
                            </div>
                          </div>
                          <Button
                            disabled={adminStore.busy}
                            onClick={() => {
                              setDraft(structuredClone(online));
                              setDirty(true);
                            }}
                          >
                            使用本次在线值
                          </Button>
                        </>
                      ) : (
                        <Typography.Text>当前在线目录没有该模型。</Typography.Text>
                      )}
                    </Space>
                  ),
                },
              ]}
            />
          </Form>
        )}
      </Card>
    </section>
  );
});
