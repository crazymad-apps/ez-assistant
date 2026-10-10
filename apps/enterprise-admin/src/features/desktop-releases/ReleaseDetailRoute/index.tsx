import { useCallback, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Alert, App, Button, Card, Descriptions, Space, Table, Tag, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { DesktopReleaseDetail, DesktopTargetDetail } from '../../../request/openapi';
import { desktopReleaseApi } from '../../../request/desktop-releases';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { platformLabels, publicationLabels } from '../model';
import { ReleaseNotesForm } from '../ReleaseNotesForm';
import styles from './index.module.scss';

export const ReleaseDetailRoute = observer(function ReleaseDetailRoute() {
  const { version = '' } = useParams();
  const navigate = useNavigate();
  const { modal } = App.useApp();
  const read = useCallback(
    (token: string, signal: AbortSignal) => desktopReleaseApi.get(token, version, signal),
    [version],
  );
  const query = useResourceQuery(read);
  const [error, setError] = useState('');
  const [failed, setFailed] = useState<DesktopReleaseDetail>();

  function publish(row: DesktopTargetDetail) {
    const published = row.publication !== 'published';
    modal.confirm({
      title: `${published ? '发布' : '撤回'} ${version} / ${platformLabels[row.target]}？`,
      content: published ? '发布后用户可发现并下载安装。' : '撤回后不再提供此平台更新，已安装的应用不受影响。',
      okText: published ? '发布' : '撤回',
      cancelText: '取消',
      okButtonProps: { danger: !published },
      onOk: async () => {
        const problem = await adminStore.manage(
          (token) =>
            desktopReleaseApi.publish(token, version, row.target, {
              manifest_sha256: row.manifest_sha256,
              expected_publication: row.publication,
              publication: published ? 'published' : 'withdrawn',
            }),
          () => {
            setError('');
            setFailed(undefined);
            query.reload();
          },
        );
        if (problem) {
          setError(problem);
          setFailed(query.value);
        }
      },
    });
  }

  return (
    <div className={styles.page}>
      <Card>
        <Space>
          <Button disabled={adminStore.busy} onClick={() => void navigate('/desktop-releases')}>
            返回安装包
          </Button>
          <Button disabled={adminStore.busy || query.loading} onClick={query.reload}>
            重新读取
          </Button>
          <Typography.Text strong>{version}</Typography.Text>
        </Space>
      </Card>
      {(query.error || error) && <Alert type="error" title={query.error || error} />}
      {query.value && (
        <>
          <Card title="更新说明">
            <ReleaseNotesForm key={version} record={query.value} loading={query.loading} reload={query.reload} />
          </Card>
          <Card title="平台制品">
            <Table<DesktopTargetDetail>
              rowKey="target"
              dataSource={query.value.targets}
              pagination={false}
              loading={query.loading}
              scroll={{ x: 520 }}
              columns={[
                {
                  title: '平台',
                  dataIndex: 'target',
                  render: (value: DesktopTargetDetail['target']) => platformLabels[value],
                },
                {
                  title: '状态',
                  dataIndex: 'publication',
                  render: (value: DesktopTargetDetail['publication']) => (
                    <Tag color={value === 'published' ? 'success' : 'default'}>{publicationLabels[value]}</Tag>
                  ),
                },
                {
                  title: '操作',
                  align: 'center',
                  fixed: 'right',
                  width: 100,
                  render: (_, row) => (
                    <Button
                      size="small"
                      danger={row.publication === 'published'}
                      disabled={adminStore.busy || query.loading || failed === query.value}
                      onClick={() => publish(row)}
                    >
                      {row.publication === 'published' ? '撤回' : '发布'}
                    </Button>
                  ),
                },
              ]}
              expandable={{
                expandedRowRender: (row) => (
                  <div className={styles.details}>
                    <Typography.Text type="success">发布清单已验证、文件一致</Typography.Text>
                    <Descriptions
                      size="small"
                      column={1}
                      items={[
                        { key: 'host', label: '随包 Host', children: row.manifest.host.version },
                        { key: 'min', label: '软件兼容下限', children: row.manifest.min_supported_version },
                        { key: 'sha', label: '清单摘要', children: row.manifest_sha256 },
                      ]}
                    />
                    {row.manifest.files.map((file) => (
                      <div key={file.name}>
                        <Typography.Text>
                          {file.name}（{(file.size / 1048576).toFixed(1)} MiB）
                        </Typography.Text>
                        <Typography.Paragraph type="secondary">{file.sha256}</Typography.Paragraph>
                        {row.publication === 'published' && (
                          <a
                            href={`/api/desktop-updates/${row.target}/releases/${version}/files/${file.purposes[0]}`}
                            download
                          >
                            下载安装文件
                          </a>
                        )}
                      </div>
                    ))}
                  </div>
                ),
              }}
            />
          </Card>
        </>
      )}
    </div>
  );
});
