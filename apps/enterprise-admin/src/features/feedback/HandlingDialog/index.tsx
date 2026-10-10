import { useCallback, useState } from 'react';
import { Alert, App, Modal, Spin, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { feedbackApi } from '../../../request/feedback';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { HandlingForm } from '../HandlingForm';

/** 列表与详情共用；打开时读取完整记录，不用列表摘要作为处理原值。 */
export const HandlingDialog = observer(function HandlingDialog({
  id,
  onClose,
  onSaved,
}: {
  id: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { modal, message } = App.useApp();
  const [dirty, setDirty] = useState(false);
  const read = useCallback((token: string, signal: AbortSignal) => feedbackApi.get(token, id, signal), [id]);
  const result = useResourceQuery(read);
  function close() {
    if (adminStore.busy) return;
    if (!dirty) onClose();
    else modal.confirm({ title: '放弃未保存的处理结果？', okText: '放弃修改', cancelText: '继续编辑', onOk: onClose });
  }
  return (
    <Modal
      open
      title="处理反馈"
      width={560}
      footer={null}
      onCancel={close}
      closable={!adminStore.busy}
      keyboard={!adminStore.busy}
      maskClosable={false}
    >
      {result.error && <Alert type="error" showIcon title={result.error} />}
      {result.loading && !result.value && <Spin />}
      {result.value && (
        <>
          <Typography.Paragraph>{result.value.title}</Typography.Paragraph>
          <HandlingForm
            key={id}
            record={result.value}
            loading={result.loading}
            reload={result.reload}
            onDirtyChange={setDirty}
            onSaved={() => {
              void message.success('处理结果已保存');
              onSaved();
              onClose();
            }}
          />
        </>
      )}
    </Modal>
  );
});
