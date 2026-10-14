import { ReleaseNotesPreview } from '../ReleaseNotesPreview';
import { useCallback, useState } from 'react';
import { Alert, Button, Form, Input, Space, Segmented, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { DesktopReleaseDetail } from '../../../request/openapi';
import { desktopReleaseApi } from '../../../request/desktop-releases';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import adminStore from '../../../stores/AdminStore';

/** 重读不覆盖正在编辑的说明；失败后的下一次写入必须显式核对新的服务器原值。 */
export const ReleaseNotesForm = observer(function ReleaseNotesForm(props: {
  record: DesktopReleaseDetail;
  loading: boolean;
  reload: () => void;
}) {
  const [base, setBase] = useState(props.record.notes);
  const [notes, setNotes] = useState(base);
  const [error, setError] = useState('');
  const [view, setView] = useState<'edit' | 'preview'>('edit');
  const published = props.record.targets.some((target) => target.first_published_at !== null);
  const [failed, setFailed] = useState<DesktopReleaseDetail>();
  const [savedSnapshot, setSavedSnapshot] = useState<DesktopReleaseDetail>();
  const discard = useCallback(() => setNotes(base), [base]);
  const dirty = notes !== base;
  useLeaveConfirmation(dirty, discard);
  const review = !!failed || (props.record !== savedSnapshot && props.record.notes !== base);
  const invalid = new TextEncoder().encode(notes).length > 16384;
  return (
    <Form
      layout="vertical"
      onFinish={async () => {
        if (!dirty || review || invalid || props.loading) return;
        setError('');
        const problem = await adminStore.manage(
          (token) => desktopReleaseApi.notes(token, props.record.version, { notes, expected_notes: base }),
          (result) => {
            setSavedSnapshot(props.record);
            setBase(result.notes);
            setNotes(result.notes);
            props.reload();
          },
        );
        if (problem) {
          setError(problem);
          setFailed(props.record);
        }
      }}
    >
      {error && <Alert type="error" title={error} />}
      {review && (
        <Alert
          type="warning"
          title="中心内容可能已变化，请重新读取并核对后继续保存"
          description={props.record.notes || '当前说明为空'}
          action={
            <Space>
              <Button onClick={props.reload} disabled={adminStore.busy || props.loading}>
                重新读取
              </Button>
              <Button
                disabled={adminStore.busy || props.loading || failed === props.record}
                onClick={() => {
                  setBase(props.record.notes);
                  setFailed(undefined);
                  setError('');
                }}
              >
                已核对，保留草稿
              </Button>
            </Space>
          }
        />
      )}
      <Typography.Paragraph type="secondary">
        {published ? '保存后更新官网与客户端的版本说明。' : '发布后在官网与客户端展示。'} 支持
        Markdown，修改后请同步仓库日志。
      </Typography.Paragraph>
      <Form.Item>
        <Segmented
          value={view}
          options={[
            { label: '编辑', value: 'edit' },
            { label: '预览', value: 'preview' },
          ]}
          onChange={setView}
          disabled={invalid}
        />
      </Form.Item>
      {view === 'preview' && <ReleaseNotesPreview notes={notes} />}
      <Form.Item
        hidden={view === 'preview'}
        label="Markdown 正文"
        htmlFor="release-notes"
        validateStatus={invalid ? 'error' : undefined}
        help={invalid ? '更新日志过长，请缩短后保存。' : undefined}
      >
        <Input.TextArea
          id="release-notes"
          placeholder="用几条简明说明，介绍本次新增、改进和修复。"
          rows={6}
          value={notes}
          disabled={adminStore.busy}
          onChange={(event) => setNotes(event.target.value)}
        />
      </Form.Item>
      <Button
        type="primary"
        htmlType="submit"
        disabled={!dirty || review || invalid || adminStore.busy || props.loading}
      >
        保存更新日志
      </Button>
    </Form>
  );
});
