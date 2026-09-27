import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router';
import {
  Alert,
  App,
  Button,
  Card,
  Collapse,
  Descriptions,
  Form,
  Input,
  Modal,
  Result,
  Space,
  Tag,
  Typography,
} from 'antd';
import { observer } from 'mobx-react-lite';
import type { PluginDescriptionInput, PluginVersionSummary } from '../../../request/openapi';
import { pluginApi } from '../../../request/plugins';
import { failure } from '../../../request/api';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { PluginUploadDialog } from '../PluginUploadDialog';
import { downloadPackage } from '../download';
import { platformLabel, publicationLabel, versionCompare } from '../model';
import styles from './index.module.scss';

export const PluginDetailRoute = observer(function PluginDetailRoute() {
  const [params] = useSearchParams();
  const name = params.get('name') ?? '';
  const navigate = useNavigate();
  const { message, modal } = App.useApp();
  const [form] = Form.useForm<PluginDescriptionInput>();
  const [editOpen, setEditOpen] = useState(false);
  const [descriptionDirty, setDescriptionDirty] = useState(false);
  const [editEtag, setEditEtag] = useState<string>();
  const [uploadOpen, setUploadOpen] = useState(false);
  const [actionError, setActionError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [downloadState, setDownloadState] = useState<{ version: string; loaded: number; total: number }>();
  const downloadController = useRef<AbortController | undefined>(undefined);
  const read = useCallback(
    (token: string, signal: AbortSignal) => (name ? pluginApi.get(token, name, signal) : Promise.resolve(null)),
    [name],
  );
  const query = useResourceQuery(read);
  useLeaveConfirmation(descriptionDirty, () => setDescriptionDirty(false));
  useEffect(() => () => downloadController.current?.abort(), []);

  function openEdit() {
    if (!query.value) return;
    form.setFieldsValue({ display_name: query.value.detail.display_name, description: query.value.detail.description });
    setEditEtag(query.value.etag);
    setConflict(false);
    setDescriptionDirty(false);
    setEditOpen(true);
  }

  function closeEdit() {
    if (descriptionDirty) {
      modal.confirm({
        title: '放弃尚未保存的修改？',
        okText: '放弃修改',
        cancelText: '继续编辑',
        onOk: () => {
          setDescriptionDirty(false);
          setEditOpen(false);
        },
      });
    } else setEditOpen(false);
  }

  async function saveDescription(values: PluginDescriptionInput) {
    if (!editEtag) {
      setActionError('缺少当前 ETag，请重新读取。');
      return;
    }
    if (conflict) return;
    setActionError('');
    const problem = await adminStore.manage(
      (token) => pluginApi.edit(token, name, values, editEtag),
      () => {
        setDescriptionDirty(false);
        setEditOpen(false);
        query.reload();
        void message.success('说明已保存');
      },
      (code) => {
        if (code === 'CONFIG_CHANGED') setConflict(true);
      },
    );
    if (problem) setActionError(problem);
  }

  async function viewLatestDescription() {
    try {
      const current = await pluginApi.get(adminStore.token, name);
      modal.confirm({
        title: '中心当前插件说明',
        width: 520,
        content: (
          <Space orientation="vertical" className={styles.full}>
            <Typography.Text>显示名称：{current.detail.display_name}</Typography.Text>
            <Input.TextArea aria-label="中心当前说明" value={current.detail.description} readOnly rows={5} />
            <Typography.Text type="secondary">原草稿仍保留，只有确认放弃后才会载入最新内容。</Typography.Text>
          </Space>
        ),
        okText: '放弃草稿并载入最新',
        cancelText: '保留草稿',
        onOk: () => {
          form.setFieldsValue({ display_name: current.detail.display_name, description: current.detail.description });
          setEditEtag(current.etag);
          setDescriptionDirty(false);
          setConflict(false);
          setActionError('');
          query.reload();
        },
      });
    } catch {
      setActionError('最新插件说明读取失败，原草稿仍保留。');
    }
  }

  function setPublication() {
    if (!query.value?.etag) {
      setActionError('缺少当前 ETag，请重新读取。');
      return;
    }
    const detail = query.value.detail;
    const etag = query.value.etag;
    const listed = detail.publication !== 'listed';
    modal.confirm({
      title: `${listed ? '上架' : '下架'}插件「${name}」？`,
      content: listed ? (
        `上架后，企业用户可发现并下载匹配的插件包。当前版本：${detail.versions.map((v) => `${v.version}（${platformLabel(v)}）`).join('；') || '无完整包'}`
      ) : (
        <Space orientation="vertical">
          <span>下架后停止下载和更新。已有安装仍可继续使用或删除。</span>
          <span>
            中心名称引用：
            {detail.dependent_agents.length ? detail.dependent_agents.join('、') : '未发现中心智能体的名称引用'}
          </span>
        </Space>
      ),
      okText: listed ? '上架' : '下架插件',
      okButtonProps: { danger: !listed, disabled: listed && detail.versions.length === 0 },
      onOk: async () => {
        const problem = await adminStore.manage(
          (token) => pluginApi.publication(token, name, listed, etag),
          () => {
            query.reload();
            void message.success(listed ? '插件已上架' : '插件已下架');
          },
          (code) => {
            if (code === 'CONFIG_CHANGED') setConflict(true);
          },
        );
        if (problem) {
          setActionError(problem);
          throw new Error(problem);
        }
      },
    });
  }

  async function download(version: PluginVersionSummary) {
    if (downloadController.current) return;
    setActionError('');
    const controller = new AbortController();
    downloadController.current = controller;
    setDownloadState({ version: version.version, loaded: 0, total: version.size_bytes });
    try {
      const outcome = await downloadPackage(adminStore.token, name, version, controller.signal, (loaded, total) =>
        setDownloadState({ version: version.version, loaded, total }),
      );
      void message.success(
        outcome === 'saved' ? '包已完整保存；运行环境仍需在目标 Host 验证。' : '下载已交给浏览器，请核对本地文件。',
      );
    } catch (error) {
      if (!controller.signal.aborted && !(error instanceof DOMException && error.name === 'AbortError'))
        setActionError(error instanceof Error && !('response' in error) ? error.message : failure(error).text);
    } finally {
      downloadController.current = undefined;
      setDownloadState(undefined);
    }
  }

  if (!name)
    return (
      <Result
        status="404"
        title="缺少插件名称"
        extra={<Button onClick={() => navigate('/plugins')}>返回插件管理</Button>}
      />
    );
  if (query.error && !query.value)
    return (
      <Result
        status="error"
        title={query.error}
        extra={
          <Space>
            <Button onClick={query.reload}>重试</Button>
            <Button onClick={() => navigate('/plugins')}>返回列表</Button>
          </Space>
        }
      />
    );
  const detail = query.value?.detail;
  const versions = detail?.versions.slice().sort((a, b) => versionCompare(a.version, b.version)) ?? [];
  return (
    <section className={styles.page}>
      <Card loading={!detail && query.loading}>
        <Space wrap>
          <Button onClick={() => navigate('/plugins')}>返回插件管理</Button>
          <Button type="primary" disabled={!detail || adminStore.busy} onClick={() => setUploadOpen(true)}>
            上传插件包
          </Button>
          <Button disabled={!detail || adminStore.busy} onClick={openEdit}>
            编辑说明
          </Button>
          <Button
            disabled={!detail || adminStore.busy}
            danger={detail?.publication === 'listed'}
            onClick={setPublication}
          >
            {detail?.publication === 'listed' ? '下架' : '上架'}
          </Button>
        </Space>
      </Card>
      {(query.error || actionError) && (
        <Alert
          type="error"
          title={query.error || actionError}
          action={
            conflict ? (
              <Button
                onClick={() => {
                  if (editOpen) void viewLatestDescription();
                  else {
                    setConflict(false);
                    query.reload();
                  }
                }}
              >
                {editOpen ? '查看最新内容' : '重新读取后确认'}
              </Button>
            ) : query.error ? (
              <Button onClick={query.reload}>重新读取</Button>
            ) : undefined
          }
        />
      )}
      {downloadState && (
        <Alert
          type="info"
          title={`正在下载 ${downloadState.version}：${downloadState.loaded} / ${downloadState.total} 字节`}
          action={<Button onClick={() => downloadController.current?.abort()}>取消下载</Button>}
        />
      )}
      {detail && (
        <>
          <Card title={detail.display_name}>
            <Descriptions
              size="small"
              column={{ xs: 1, sm: 2 }}
              items={[
                { key: 'name', label: '编码', children: detail.name },
                { key: 'publication', label: '上架情况', children: <Tag>{publicationLabel(detail.publication)}</Tag> },
                { key: 'description', label: '说明', span: 2, children: detail.description || '—' },
                {
                  key: 'agents',
                  label: '引用的智能体',
                  span: 2,
                  children: detail.dependent_agents.length
                    ? detail.dependent_agents.join('、')
                    : '未发现中心智能体的名称引用',
                },
              ]}
            />
          </Card>
          <Card title="版本与当前完整包">
            {versions.length === 0 ? (
              <Typography.Text type="secondary">暂无完整包</Typography.Text>
            ) : (
              <Collapse
                defaultActiveKey={[versions[0]!.version]}
                items={versions.map((version) => ({
                  key: version.version,
                  label: `版本 ${version.version} · ${platformLabel(version)}`,
                  children: (
                    <Space orientation="vertical" className={styles.full}>
                      <Descriptions
                        size="small"
                        column={{ xs: 1, sm: 2 }}
                        items={[
                          { key: 'platforms', label: '支持平台', children: platformLabel(version) },
                          { key: 'host', label: '最低 Host 版本', children: version.min_host_version },
                          {
                            key: 'format',
                            label: '格式',
                            children: version.archive_format === 'zip' ? 'ZIP' : 'TAR.GZ',
                          },
                          {
                            key: 'size',
                            label: '包大小',
                            children: `${(version.size_bytes / 1024 / 1024).toFixed(1)} MiB`,
                          },
                          {
                            key: 'updated',
                            label: '当前包上传时间',
                            children: new Date(version.updated_at).toLocaleString(),
                          },
                          {
                            key: 'sha',
                            label: 'SHA-256',
                            children: <Typography.Text copyable>{version.sha256}</Typography.Text>,
                          },
                          { key: 'skills', label: 'Skill', span: 2, children: version.skills.join('、') || '无' },
                          { key: 'mcp', label: 'MCP', span: 2, children: version.mcp.join('、') || '无' },
                          {
                            key: 'config',
                            label: '配置需求',
                            span: 2,
                            children:
                              version.configuration
                                .map(
                                  (item) =>
                                    `${item.label} (${item.name}${item.required ? '，必填' : ''}${item.secret ? '，秘密' : ''})`,
                                )
                                .join('；') || '无',
                          },
                          {
                            key: 'software',
                            label: '外部软件',
                            span: 2,
                            children:
                              version.external_software
                                .map((item) => `${item.display_name}：${item.instructions}`)
                                .join('；') || '无',
                          },
                        ]}
                      />
                      <Button
                        disabled={detail.publication !== 'listed' || Boolean(downloadState)}
                        onClick={() => void download(version)}
                      >
                        下载完整包
                      </Button>
                      {detail.publication !== 'listed' && (
                        <Typography.Text type="secondary">
                          {detail.publication === 'deprecated' ? '插件已下架，无法下载。' : '上架后可下载。'}
                        </Typography.Text>
                      )}
                    </Space>
                  ),
                }))}
              />
            )}
          </Card>
        </>
      )}
      <Modal open={editOpen} title="编辑插件说明" width={520} onCancel={closeEdit} footer={null} maskClosable={false}>
        {conflict && (
          <Alert
            type="warning"
            title="中心说明已变化，原草稿仍保留。"
            action={<Button onClick={() => void viewLatestDescription()}>查看最新内容</Button>}
          />
        )}
        <Form
          form={form}
          layout="vertical"
          disabled={adminStore.busy}
          onValuesChange={() => setDescriptionDirty(true)}
          onFinish={(values) => void saveDescription(values)}
        >
          <Form.Item label="编码">
            <Typography.Text>{name}</Typography.Text>
          </Form.Item>
          <Form.Item name="display_name" label="显示名称" rules={[{ required: true, whitespace: true }]}>
            <Input placeholder="输入插件显示名称" />
          </Form.Item>
          <Form.Item name="description" label="说明">
            <Input.TextArea placeholder="输入插件说明" rows={5} />
          </Form.Item>
          <Space>
            <Button type="primary" htmlType="submit" loading={adminStore.busy} disabled={conflict}>
              保存修改
            </Button>
            <Button onClick={closeEdit}>取消</Button>
          </Space>
        </Form>
      </Modal>
      <PluginUploadDialog
        open={uploadOpen}
        onClose={() => setUploadOpen(false)}
        previousShaFor={(uploadedName, version) =>
          uploadedName === name ? detail?.versions.find((item) => item.version === version)?.sha256 : undefined
        }
        onUploaded={() => query.reload()}
        onInspect={() => query.reload()}
      />
    </section>
  );
});
