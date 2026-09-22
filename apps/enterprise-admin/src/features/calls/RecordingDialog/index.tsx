import { useState } from 'react';
import { Alert, App, Form, InputNumber, Modal, Switch } from 'antd';
import { observer } from 'mobx-react-lite';
import type { RecordingSettings } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';

export const RecordingDialog = observer(function RecordingDialog(
  props: Readonly<{ initial: RecordingSettings; onClose: () => void; onSaved: () => void }>,
) {
  const [draft, setDraft] = useState(props.initial);
  const [error, setError] = useState('');
  const { modal } = App.useApp();

  async function save() {
    if (draft.content_retention_days < 1 || draft.index_retention_days < draft.content_retention_days) {
      setError('正文保留期须大于 0，且不能超过索引保留期。');
      return;
    }
    const confirm =
      (!props.initial.enabled && draft.enabled) ||
      draft.content_retention_days < props.initial.content_retention_days ||
      draft.index_retention_days < props.initial.index_retention_days;
    if (
      confirm &&
      !(await modal.confirm({
        title: '确认修改采集策略？',
        content: '启用后将保存可能包含敏感信息的模型正文；缩短保留期会在清理时删除到期内容，无法撤销。',
      }))
    )
      return;
    const failure = await adminStore.saveRecordingSettings(draft);
    if (failure) setError(failure);
    else {
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
      confirmLoading={adminStore.busy}
      cancelButtonProps={{ disabled: adminStore.busy }}
      closable={!adminStore.busy}
      maskClosable={false}
    >
      {error && <Alert type="error" title={error} />}
      <Form layout="vertical">
        <Form.Item label="采集请求和响应正文">
          <Switch
            checked={draft.enabled}
            disabled={adminStore.busy}
            onChange={(enabled) => setDraft({ ...draft, enabled })}
          />
        </Form.Item>
        <Form.Item label="正文保留天数">
          <InputNumber
            min={1}
            max={3650}
            precision={0}
            value={draft.content_retention_days}
            placeholder="请输入天数"
            onChange={(value) => setDraft({ ...draft, content_retention_days: value ?? 1 })}
          />
        </Form.Item>
        <Form.Item label="索引保留天数">
          <InputNumber
            min={1}
            max={3650}
            precision={0}
            value={draft.index_retention_days}
            placeholder="请输入天数"
            onChange={(value) => setDraft({ ...draft, index_retention_days: value ?? 1 })}
          />
        </Form.Item>
      </Form>
      <Alert type="info" title="每侧最多保存 8 MiB，中心采集缓冲共 64 MiB。关闭采集保留已有文件；调用索引持续记录。" />
    </Modal>
  );
});
