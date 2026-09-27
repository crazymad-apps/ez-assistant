import { useRef, useState } from 'react';
import { Alert, App, Button, Modal, Progress, Space, Typography } from 'antd';
import type { PackageUploadResult } from '../../../request/openapi';
import { pluginApi } from '../../../request/plugins';
import adminStore from '../../../stores/AdminStore';
import styles from './index.module.scss';

type Phase = 'selected' | 'sending' | 'checking' | 'success' | 'unknown' | 'failed';
type Props = Readonly<{
  open: boolean;
  onClose: () => void;
  onUploaded: (result: PackageUploadResult) => void;
  onInspect: (name?: string) => void;
  previousShaFor: (name: string, version: string) => string | undefined;
}>;

/** 传输字节、中心校验和发布结果分开呈现；取消不能推断事务已回滚。 */
export function PluginUploadDialog(props: Props) {
  const { modal } = App.useApp();
  const input = useRef<HTMLInputElement>(null);
  const request = useRef<AbortController | undefined>(undefined);
  const completed = useRef(false);
  const [file, setFile] = useState<File>();
  const [phase, setPhase] = useState<Phase>('selected');
  const [sent, setSent] = useState(0);
  const [error, setError] = useState('');
  const [result, setResult] = useState<PackageUploadResult>();
  const [previousSha, setPreviousSha] = useState<string>();

  function select(next?: File) {
    if (!next) return;
    if (!/\.(zip|tar\.gz)$/i.test(next.name) || next.size === 0 || next.size > 2 * 1024 * 1024 * 1024) {
      setError('请选择不超过 2 GiB 的 .zip 或 .tar.gz 完整包。');
      return;
    }
    setFile(next);
    setPhase('selected');
    setResult(undefined);
    setPreviousSha(undefined);
    setSent(0);
    setError('');
  }

  async function upload() {
    if (!file || adminStore.busy) return;
    const controller = new AbortController();
    request.current = controller;
    completed.current = false;
    setSent(0);
    setError('');
    setPhase('sending');
    let unknownResult = false;
    const problem = await adminStore.manage(
      (token) =>
        pluginApi.upload(token, file, controller.signal, (loaded) => {
          setSent(loaded);
          if (loaded >= file.size) setPhase('checking');
        }),
      (value) => {
        completed.current = true;
        setPreviousSha(props.previousShaFor(value.name, value.version));
        setResult(value);
        setPhase('success');
        props.onUploaded(value);
      },
      (code) => {
        unknownResult = code === 'PACKAGE_RESULT_UNKNOWN' || code === 'PACKAGE_TIMEOUT' || code === 'PACKAGE_CANCELLED';
      },
    );
    if (problem && !controller.signal.aborted) {
      setError(problem);
      setPhase(unknownResult ? 'unknown' : 'failed');
    }
    if (controller.signal.aborted && !completed.current) setPhase('unknown');
    request.current = undefined;
  }

  function resetAndClose() {
    setFile(undefined);
    setResult(undefined);
    setPreviousSha(undefined);
    setError('');
    setSent(0);
    setPhase('selected');
    props.onClose();
  }

  function cancel() {
    if (phase === 'sending' || phase === 'checking') {
      modal.confirm({
        title: '停止本次上传？',
        content: '若中心已接收完整包，仍需核对最终结果；停止等待不表示发布已回滚。',
        okText: '停止等待',
        cancelText: '继续等待',
        onOk: () => {
          if (completed.current || !request.current) return;
          request.current?.abort();
          setPhase('unknown');
        },
      });
      return;
    }
    resetAndClose();
  }

  const active = phase === 'sending' || phase === 'checking';
  return (
    <Modal
      open={props.open}
      title="上传插件包"
      width={680}
      onCancel={cancel}
      closable={!active}
      maskClosable={false}
      keyboard={!active}
      footer={
        <Space>
          {phase === 'success' && (
            <Button
              onClick={() => {
                props.onInspect(result?.name);
                resetAndClose();
              }}
            >
              查看插件
            </Button>
          )}
          {phase === 'unknown' && <Button onClick={() => props.onInspect(result?.name)}>核对结果</Button>}
          <Button onClick={cancel}>{active ? '取消上传' : '关闭'}</Button>
          {(phase === 'selected' || phase === 'failed') && (
            <Button type="primary" disabled={!file || adminStore.busy} onClick={() => void upload()}>
              上传
            </Button>
          )}
        </Space>
      }
    >
      <div className={styles.content}>
        <div
          className={styles.drop}
          role="button"
          tabIndex={active ? -1 : 0}
          aria-label="选择或拖放插件包"
          onClick={() => {
            if (!active) input.current?.click();
          }}
          onKeyDown={(event) => {
            if (!active && (event.key === 'Enter' || event.key === ' ')) input.current?.click();
          }}
          onDragOver={(event) => event.preventDefault()}
          onDrop={(event) => {
            event.preventDefault();
            if (!active) select(event.dataTransfer.files[0]);
          }}
        >
          <input
            ref={input}
            type="file"
            accept=".zip,.tar.gz,application/zip,application/gzip"
            hidden
            onChange={(event) => {
              select(event.target.files?.[0]);
              event.target.value = '';
            }}
          />
          {file ? `${file.name} · ${(file.size / 1024 / 1024).toFixed(1)} MiB` : '选择或拖放 .zip / .tar.gz 插件包'}
        </div>
        <Typography.Text type="secondary">
          包需包含声明的运行环境和依赖；Blender 等外部软件在包中声明。支持平台由中心解析包内清单，不在此勾选。
        </Typography.Text>
        {file && phase !== 'success' && (
          <Alert type="warning" title="同名同版本的不同内容将替换中心当前包；已安装客户端不会收到同版本更新。" />
        )}
        {active && (
          <>
            <Progress percent={file ? Math.min(100, Math.round((sent / file.size) * 100)) : 0} />
            <Typography.Text>
              {phase === 'checking' ? '字节已发送，中心正在校验和保存；尚未确认发布结果。' : '正在上传原始包字节。'}
            </Typography.Text>
          </>
        )}
        {phase === 'success' && result && (
          <Alert
            type="success"
            title={
              result.result === 'created'
                ? '插件包已上传'
                : result.result === 'replaced'
                  ? `已替换版本 ${result.version} 的中心当前包`
                  : '该插件包已存在，中心内容未变化'
            }
            description={
              <Space orientation="vertical">
                <span>
                  {result.name} · {result.version} · {result.platforms.map((p) => `${p.os}-${p.arch}`).join('、')}
                </span>
                {result.result === 'replaced' && previousSha && <span>旧 SHA：{previousSha}</span>}
                <span>当前 SHA：{result.sha256}</span>
                {result.result === 'replaced' && <span>客户端不会收到同版本更新。</span>}
              </Space>
            }
          />
        )}
        {phase === 'unknown' && (
          <Alert type="warning" title="上传结果未确认，请按名称、版本和当前 SHA 核对中心记录。不要自动重传。" />
        )}
        {error && <Alert type="error" title={error} />}
      </div>
    </Modal>
  );
}
