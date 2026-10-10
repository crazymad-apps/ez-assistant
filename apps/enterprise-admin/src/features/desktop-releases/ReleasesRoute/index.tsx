import { useCallback, useEffect, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, Button, Card, Space, Table, Tag } from 'antd';
import { observer } from 'mobx-react-lite';
import type { DesktopReleaseSummary } from '../../../request/openapi';
import { desktopReleaseApi } from '../../../request/desktop-releases';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { ReleaseUploadDialog } from '../ReleaseUploadDialog';
import { platformLabels, publicationLabels, releasePath } from '../model';

export const ReleasesRoute = observer(function ReleasesRoute() {
  const navigate = useNavigate();
  const [upload, setUpload] = useState(false);
  const [uploadedVersion, setUploadedVersion] = useState<string>();
  const busy = adminStore.busy;
  useEffect(() => {
    // 完成请求并卸载上传编辑器后再导航，让离开守卫先释放本次编辑登记。
    if (uploadedVersion && !busy) {
      setUploadedVersion(undefined);
      void navigate(releasePath(uploadedVersion));
    }
  }, [uploadedVersion, busy, navigate]);
  const read = useCallback((token: string, signal: AbortSignal) => desktopReleaseApi.list(token, signal), []);
  const query = useResourceQuery(read);
  return (
    <Card
      title="安装包"
      extra={
        <Button type="primary" disabled={adminStore.busy} onClick={() => setUpload(true)}>
          上传安装包
        </Button>
      }
    >
      {query.error && (
        <Alert type="error" title={query.error} action={<Button onClick={query.reload}>重新读取</Button>} />
      )}
      <Table<DesktopReleaseSummary>
        rowKey="version"
        loading={query.loading}
        dataSource={query.value ?? []}
        pagination={false}
        scroll={{ x: 850 }}
        locale={{ emptyText: '暂无安装包' }}
        columns={[
          { title: '版本', dataIndex: 'version', width: 120 },
          { title: '更新说明', dataIndex: 'notes', ellipsis: true, render: (value) => value || '尚未填写' },
          {
            title: '平台发布情况',
            width: 380,
            render: (_, row) => (
              <Space wrap>
                {row.targets.map((item) => (
                  <Tag key={item.target} color={item.publication === 'published' ? 'success' : 'default'}>
                    {platformLabels[item.target]}：{publicationLabels[item.publication]}
                  </Tag>
                ))}
              </Space>
            ),
          },
          {
            title: '操作',
            width: 90,
            fixed: 'right',
            align: 'center',
            render: (_, row) => (
              <Button type="link" size="small" onClick={() => void navigate(releasePath(row.version))}>
                详情
              </Button>
            ),
          },
        ]}
      />
      {upload && (
        <ReleaseUploadDialog
          onClose={() => setUpload(false)}
          onUploaded={(version) => {
            setUpload(false);
            setUploadedVersion(version);
          }}
        />
      )}
    </Card>
  );
});
