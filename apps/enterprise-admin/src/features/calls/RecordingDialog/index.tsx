import { useState, useCallback, useEffect } from 'react';
import { Alert, App, Button, Form, InputNumber, Modal, Radio } from 'antd';
import { observer } from 'mobx-react-lite';
import type { RecordingSettings } from '../../../request/openapi';
import { callApi } from '../../../request/calls';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { recordingLabels } from '../model';

export const RecordingDialog = observer(function RecordingDialog(
  props: Readonly<{ initial: RecordingSettings; onClose: () => void; onSaved: () => void }>,
) {
  const [readVersion, setReadVersion] = useState(0);
  const read = useCallback(
    (token: string, signal: AbortSignal) => {
      void readVersion;
      return callApi.settings(token, signal);
    },
    [readVersion],
  );
  const current = useResourceQuery(read);
  const [initialized, setInitialized] = useState(false);
  const [draft, setDraft] = useState(props.initial);
  useEffect(() => {
    if (!initialized && current.value) {
      setDraft(current.value);
      setInitialized(true);
    }
  }, [initialized, current.value]);
  const [error, setError] = useState('');
  const [reconcile, setReconcile] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const { modal, message } = App.useApp();
  const baseline = current.value;
  const changed =
    baseline &&
    (draft.mode !== baseline.mode ||
      draft.content_retention_days !== baseline.content_retention_days ||
      draft.index_retention_days !== baseline.index_retention_days);
  const contentInvalid = !Number.isInteger(draft.content_retention_days) || draft.content_retention_days < 1;
  const indexInvalid =
    !Number.isInteger(draft.index_retention_days) ||
    draft.index_retention_days < draft.content_retention_days ||
    draft.index_retention_days > 3650;
  const locked = adminStore.busy || confirming;
  const draftLocked = locked || !initialized;

  async function save() {
    if (!baseline || !initialized || !changed || contentInvalid || indexInvalid || reconcile || locked) return;
    const expanded =
      (baseline.mode === 'off' && draft.mode !== 'off') || (baseline.mode === 'anomalies' && draft.mode === 'all');
    const shortened =
      draft.content_retention_days < baseline.content_retention_days ||
      draft.index_retention_days < baseline.index_retention_days;
    if (expanded || shortened) {
      setConfirming(true);
      const accepted = await modal.confirm({
        title: '确认修改采集策略？',
        content: `模式：${recordingLabels[baseline.mode]} → ${recordingLabels[draft.mode]}。正文保留 ${baseline.content_retention_days} → ${draft.content_retention_days} 天，索引保留 ${baseline.index_retention_days} → ${draft.index_retention_days} 天。正文可能包含敏感信息。新模式仅影响新调用；缩短期限会清理已有到期记录，无法撤销。`,
      });
      setConfirming(false);
      if (!accepted) return;
    }
    const failure = await adminStore.saveRecordingSettings(draft);
    if (failure) {
      setError(failure);
      // 写入失败可能已提交：禁止自动补发，明确回读并由用户核对后恢复保存。
      setReconcile(true);
      setReadVersion((version) => version + 1);
    } else {
      void message.success('记录策略已保存');
      props.onSaved();
      props.onClose();
    }
  }

  return (
    <Modal
      open
      title="调用记录策略"
      onCancel={props.onClose}
      onOk={save}
      confirmLoading={locked}
      okButtonProps={{
        disabled:
          !initialized || !changed || contentInvalid || indexInvalid || reconcile || current.loading || !!current.error,
      }}
      cancelButtonProps={{ disabled: locked }}
      closable={!locked}
      maskClosable={false}
    >
      {(error || current.error) && <Alert type="error" title={error || current.error} />}
      {reconcile && (
        <Alert
          type="warning"
          title="保存结果需核对，请先读取当前策略。"
          description={
            baseline && !current.loading && !current.error
              ? `当前：${recordingLabels[baseline.mode]}，正文 ${baseline.content_retention_days} 天，索引 ${baseline.index_retention_days} 天。`
              : undefined
          }
          action={
            <Button
              disabled={current.loading || (!current.value && !current.error)}
              onClick={() => {
                if (current.error) current.reload();
                else {
                  setReconcile(false);
                  setError('');
                }
              }}
            >
              {current.error ? '重新读取' : '已核对，继续编辑'}
            </Button>
          }
        />
      )}
      {!reconcile && current.error && <Button onClick={current.reload}>重新读取</Button>}
      <Form layout="vertical" disabled={draftLocked}>
        <Form.Item label="正文留存模式">
          <Radio.Group
            value={draft.mode}
            onChange={(event) => setDraft({ ...draft, mode: event.target.value })}
            options={Object.entries(recordingLabels).map(([value, label]) => ({ value, label }))}
          />
        </Form.Item>
        <Form.Item
          label="正文保留天数"
          validateStatus={contentInvalid ? 'error' : undefined}
          help={contentInvalid ? '请输入至少 1 天的整数。' : undefined}
        >
          <InputNumber
            min={1}
            max={3650}
            precision={0}
            value={draft.content_retention_days}
            onChange={(value) => setDraft({ ...draft, content_retention_days: value ?? 0 })}
          />
        </Form.Item>
        <Form.Item
          label="索引保留天数"
          validateStatus={indexInvalid ? 'error' : undefined}
          help={indexInvalid ? '索引保留期不能小于正文保留期，最多 3650 天。' : undefined}
        >
          <InputNumber
            min={1}
            max={3650}
            precision={0}
            value={draft.index_retention_days}
            onChange={(value) => setDraft({ ...draft, index_retention_days: value ?? 0 })}
          />
        </Form.Item>
      </Form>
      <Alert
        type="info"
        title="仅异常与未确认模式会丢弃已确认正常的正文，不能覆盖所有仅在客户端运行时发生的错误。关闭采集不立即删除历史正文。"
      />
    </Modal>
  );
});
