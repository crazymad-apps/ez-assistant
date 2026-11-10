import { useState } from 'react';
import { Button } from '../../components/Button';
import { Dialog } from '../../components/Dialog';
import { ApprovalInput } from '../composer/ComposerDock/ApprovalWorkspace';
import type { AppToolPrompt } from './appTools';
import styles from './appDialogs.module.scss';

export function AppToolApprovalDialog({ prompt }: { prompt: AppToolPrompt }) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState(false);
  async function decide(allow: boolean) {
    if (pending) return;
    setPending(true);
    setError(false);
    try {
      await prompt.decide(allow);
    } catch {
      setPending(false);
      setError(true);
    }
  }
  return (
    <Dialog
      aria_label="应用工具授权"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      dismissible={!pending}
      on_close={() => void decide(false)}
    >
      <h2>允许应用调用工具？</h2>
      <p>来自应用：{prompt.approval.app_title}</p>
      <p>{prompt.approval.scope.type === 'session' ? '当前会话' : '当前草稿'} · 仅本次</p>
      <ApprovalInput input={prompt.approval.input} />
      {error && <p role="alert">授权请求已失效或暂不可用。</p>}
      <div className={styles.actions}>
        <Button variant="text" disabled={pending} onClick={() => void decide(false)}>拒绝</Button>
        <Button disabled={pending} onClick={() => void decide(true)}>允许本次</Button>
      </div>
    </Dialog>
  );
}
