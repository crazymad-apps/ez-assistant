import { useCallback, useEffect, useState } from 'react';
import { Alert, Button, Form, Input, Select, Space, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import type { FeedbackDetail } from '../../../request/openapi';
import { feedbackApi } from '../../../request/feedback';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { statusLabels, statusOptions } from '../model';
import styles from './index.module.scss';

/** 草稿独立于服务器重读；冲突必须核对后再以新原值保存。 */
export const HandlingForm = observer(function HandlingForm({
  record,
  loading,
  reload,
  onDirtyChange,
  onSaved,
}: {
  record: FeedbackDetail;
  loading: boolean;
  reload: () => void;
  onDirtyChange: (dirty: boolean) => void;
  onSaved: () => void;
}) {
  const [base, setBase] = useState(record);
  const [status, setStatus] = useState(record.status);
  const [note, setNote] = useState(record.handling_note ?? '');
  const [error, setError] = useState('');
  const [saved, setSaved] = useState(false);
  const [failedSnapshot, setFailedSnapshot] = useState<FeedbackDetail>();
  const [savedSnapshot, setSavedSnapshot] = useState<FeedbackDetail>();
  const dirty = status !== base.status || (note.trim() || null) !== base.handling_note;
  useEffect(() => {
    onDirtyChange(dirty);
  }, [dirty, onDirtyChange]);
  // 保存回执先于重读到达时，旧投影不能冒充他人刚发生的修改。
  const changed =
    record !== savedSnapshot && (record.status !== base.status || record.handling_note !== base.handling_note);
  const review = changed || !!failedSnapshot;
  const discard = useCallback(() => {
    setStatus(base.status);
    setNote(base.handling_note ?? '');
  }, [base]);
  useLeaveConfirmation(dirty, discard);
  const length = [...note.trim()].length;
  const invalid = length > 2000 || ((status === 'resolved' || status === 'closed') && !note.trim());
  return (
    <Form
      layout="vertical"
      onFinish={async () => {
        if (invalid || review || loading) return;
        setError('');
        setSaved(false);
        const problem = await adminStore.manage(
          (token) =>
            feedbackApi.handle(token, record.id, {
              expected_status: base.status,
              expected_handling_note: base.handling_note,
              status,
              handling_note: note.trim() || null,
            }),
          (result) => {
            setSavedSnapshot(record);
            setBase(result);
            setStatus(result.status);
            setNote(result.handling_note ?? '');
            setSaved(true);
            onSaved();
          },
        );
        if (problem) {
          setError(problem);
          setFailedSnapshot(record);
        }
      }}
    >
      {error && <Alert type="error" showIcon title={error} />}
      {saved && !review && <Alert type="success" showIcon title="处理结果已保存" />}
      {review && (
        <Space orientation="vertical">
          <Alert
            type="warning"
            showIcon
            title="请核对中心当前处理结果，草稿已保留"
            description={
              <>
                <div>
                  {failedSnapshot === record ? '上次读取状态' : '当前状态'}：{statusLabels[record.status]}
                </div>
                <Typography.Paragraph className={styles.note}>
                  {failedSnapshot === record ? '上次读取说明' : '当前说明'}：{record.handling_note ?? '暂无说明'}
                </Typography.Paragraph>
              </>
            }
          />
          <Space>
            <Button onClick={reload} disabled={loading || adminStore.busy}>
              重新读取
            </Button>
            <Button
              disabled={loading || adminStore.busy || failedSnapshot === record}
              onClick={() => {
                setBase(record);
                setFailedSnapshot(undefined);
                setError('');
                setSaved(false);
              }}
            >
              已核对，保留草稿继续编辑
            </Button>
          </Space>
        </Space>
      )}
      <Form.Item label="处理状态" htmlFor="feedback-status">
        <Select
          id="feedback-status"
          value={status}
          options={statusOptions}
          disabled={adminStore.busy}
          onChange={(value) => {
            setStatus(value);
            setSaved(false);
          }}
        />
      </Form.Item>
      <Form.Item
        label="处理说明"
        htmlFor="feedback-note"
        required={status === 'resolved' || status === 'closed'}
        validateStatus={invalid ? 'error' : undefined}
        help={invalid ? '已解决或已关闭须填写说明，最多 2000 字。' : '提交者可以查看处理说明。'}
      >
        <Input.TextArea
          id="feedback-note"
          rows={5}
          value={note}
          disabled={adminStore.busy}
          onChange={(event) => {
            setNote(event.target.value);
            setSaved(false);
          }}
        />
      </Form.Item>
      <Space>
        <Button
          type="primary"
          htmlType="submit"
          loading={adminStore.busy}
          disabled={!dirty || invalid || review || loading}
        >
          保存处理结果
        </Button>
        <Button disabled={!dirty || adminStore.busy} onClick={discard}>
          撤销草稿修改
        </Button>
      </Space>
    </Form>
  );
});
