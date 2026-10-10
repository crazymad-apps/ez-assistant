import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, App, Button, Modal, Progress, Space, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { failure } from '../../../request/api';
import adminStore from '../../../stores/AdminStore';
import { desktopReleaseApi } from '../../../request/desktop-releases';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';
import { platformLabels, selectRelease } from '../model';
import styles from './index.module.scss';

type Selection = Awaited<ReturnType<typeof selectRelease>>;

export const ReleaseUploadDialog = observer(function ReleaseUploadDialog(props: {
  onClose: () => void;
  onUploaded: (version: string) => void;
}) {
  const { modal } = App.useApp();
  const [selection, setSelection] = useState<Selection>();
  const [phase, setPhase] = useState<'selected' | 'sending' | 'unknown' | 'checking'>('selected');
  const [percent, setPercent] = useState(0);
  const [error, setError] = useState('');
  const request = useRef<AbortController | undefined>(undefined);
  const generation = useRef(0);
  const mounted = useRef(true);
  const input = useRef<HTMLInputElement>(null);
  const active = phase === 'sending' || phase === 'checking';
  const discard = useCallback(() => props.onClose(), [props.onClose]);
  useLeaveConfirmation(!!selection, discard);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current++;
      request.current?.abort();
    };
  }, []);

  async function select(files: File[]) {
    const owner = ++generation.current;
    setError('');
    setSelection(undefined);
    try {
      const value = await selectRelease(files);
      if (mounted.current && owner === generation.current) {
        setSelection(value);
        setPhase('selected');
      }
    } catch (error) {
      if (mounted.current && owner === generation.current)
        setError(error instanceof Error ? error.message : '无法读取发布清单。');
    }
  }

  async function upload() {
    if (!selection || adminStore.busy || phase !== 'selected') return;
    const controller = new AbortController();
    request.current = controller;
    setError('');
    setPercent(0);
    setPhase('sending');
    const problem = await adminStore.manage(
      (token) =>
        desktopReleaseApi.upload(
          token,
          selection.text,
          selection.signature,
          selection.artifacts,
          controller.signal,
          (value) => {
            if (mounted.current) {
              setPercent(value);
              if (value === 100) setPhase('checking');
            }
          },
        ),
      (result) => {
        if (mounted.current) props.onUploaded(result.version);
      },
    );
    request.current = undefined;
    if (problem && mounted.current) {
      setError(problem);
      setPhase('unknown');
    }
  }

  async function inspect() {
    if (!selection || adminStore.busy) return;
    const token = adminStore.token;
    try {
      const detail = await desktopReleaseApi.get(token, selection.version);
      if (!mounted.current || token !== adminStore.token) return;
      if (detail.targets.some((row) => row.target === selection.target && row.manifest_text === selection.text))
        props.onUploaded(selection.version);
      else {
        setError('中心当前文件与本次选择不同。已核对，可重新上传草稿。');
        setPhase('selected');
      }
    } catch (error) {
      if (!mounted.current || token !== adminStore.token) return;
      const problem = failure(error);
      if (problem.code === 'DESKTOP_RELEASE_NOT_FOUND') {
        setError('中心尚无该版本，已核对，可重新上传。');
        setPhase('selected');
      } else setError(problem.text);
    }
  }

  function cancel() {
    if (!active) {
      props.onClose();
      return;
    }
    modal.confirm({
      title: '停止本次上传？',
      content: '停止等待不代表中心已回滚；稍后需要重新读取结果。',
      okText: '停止等待',
      cancelText: '继续等待',
      onOk: () => request.current?.abort(),
    });
  }

  return (
    <Modal
      open
      title="上传安装包"
      width={600}
      onCancel={cancel}
      maskClosable={false}
      keyboard={!active}
      closable={!active}
      footer={
        <Space>
          <Button onClick={cancel}>{active ? '取消上传' : '关闭'}</Button>
          {phase === 'unknown' ? (
            <Button disabled={adminStore.busy} onClick={() => void inspect()}>
              重新读取结果
            </Button>
          ) : (
            <Button type="primary" disabled={!selection || adminStore.busy || active} onClick={() => void upload()}>
              上传
            </Button>
          )}
        </Space>
      }
    >
      <div className={styles.content}>
        <input
          ref={input}
          type="file"
          multiple
          hidden
          onChange={(event) => {
            void select(Array.from(event.target.files ?? []));
            event.target.value = '';
          }}
        />
        <Button disabled={active || phase === 'unknown'} onClick={() => input.current?.click()}>
          选择发布文件
        </Button>
        <Typography.Text type="secondary">选择同一平台的 release.json、release.json.sig 和安装文件。</Typography.Text>
        {selection && (
          <>
            <Typography.Text strong>
              {selection.version} / {platformLabels[selection.target]}
            </Typography.Text>
            {selection.artifacts.map((file) => (
              <Typography.Text key={file.name}>
                {file.name}（{(file.size / 1048576).toFixed(1)} MiB）
              </Typography.Text>
            ))}
          </>
        )}
        {active && (
          <>
            <Progress percent={percent} />
            <span role="status">{phase === 'checking' ? '正在保存…' : '正在上传…'}</span>
          </>
        )}
        {phase === 'unknown' && <Alert type="warning" title="结果尚未确认，请重新读取" />}
        {error && <Alert type="error" title={error} />}
      </div>
    </Modal>
  );
});
