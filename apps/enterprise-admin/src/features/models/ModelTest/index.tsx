import { useEffect, useRef, useState } from 'react';
import { Alert, Button, Space, Typography } from 'antd';
import { Link } from 'react-router';
import { observer } from 'mobx-react-lite';
import type { ModelSelection, ModelTestResult } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';

export const ModelTest = observer(function ModelTest(
  props: Readonly<{ selection: ModelSelection; disabled: boolean }>,
) {
  const active = useRef<AbortController | undefined>(undefined);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<ModelTestResult>();
  const [error, setError] = useState('');
  useEffect(
    () => () => {
      active.current?.abort();
    },
    [],
  );

  async function test() {
    const request = new AbortController();
    active.current = request;
    setRunning(true);
    setResult(undefined);
    setError('');
    const failure = await adminStore.testModel(props.selection, request.signal, (result) => {
      if (!request.signal.aborted) setResult(result);
    });
    if (request.signal.aborted) return;
    setRunning(false);
    if (failure) setError(failure);
  }

  function cancel() {
    active.current?.abort();
    setRunning(false);
    setError('已停止等待，调用可能已产生费用；可在调用记录中核对结果。');
  }

  return (
    <Space orientation="vertical">
      <Space>
        <Button disabled={props.disabled || adminStore.busy} loading={running} onClick={test}>
          测试已保存配置
        </Button>
        {running && <Button onClick={cancel}>取消等待</Button>}
      </Space>
      <Typography.Text type="secondary">短文本连通性测试，可能产生费用；不代表全部模型能力已验证。</Typography.Text>
      {error && <Alert type="warning" title={error} />}
      {result && (
        <Alert
          type={result.connected ? 'success' : 'warning'}
          title={result.connected ? '连通性测试通过' : `测试未通过：${result.reason ?? '请查看记录'}`}
          description={<Link to={`/calls/${result.call_id}`}>查看调用 {result.call_id}</Link>}
        />
      )}
    </Space>
  );
});
