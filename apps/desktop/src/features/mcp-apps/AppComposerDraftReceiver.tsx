import { useEffect, useRef, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { useRootStore } from '../../stores/RootStoreContext';
import { Dialog } from '../../components/Dialog';
import { Button } from '../../components/Button';
import styles from './appDialogs.module.scss';

type Proposal = {
  cleanup: () => void;
  text: string;
  previous: string;
  active: () => boolean;
  resolve: () => void;
  reject: (error: Error) => void;
};
/** 现有 Composer 仍持有草稿；这里只管理一次替换请求，不复制正文或自动发送。 */
export const AppComposerDraftReceiver = observer(function AppComposerDraftReceiver({
  text,
  has_other_content,
  on_fill,
}: {
  text: string;
  has_other_content: boolean;
  on_fill: (text: string) => void;
}) {
  const root = useRootStore();
  const resources = root.resource_workspace;
  const scope = root.reading_scope;
  const latest = useRef({ text, has_other_content, on_fill });
  latest.current = { text, has_other_content, on_fill };
  const pending = useRef<Proposal | null>(null);
  const [proposal, setProposal] = useState<Proposal | null>(null);
  useEffect(() => {
    const handler = {
      resources,
      scope,
      request: async (value: string, active: () => boolean, signal: AbortSignal) => {
        if (!active() || pending.current) throw new Error('草稿请求已失效或正在确认');
        const current = latest.current;
        if (!current.text.trim() && !current.has_other_content) {
          current.on_fill(value);
          return;
        }
        await new Promise<void>((resolve, reject) => {
          const cancel = () => {
            if (pending.current === proposal) {
              pending.current = null;
              setProposal(null);
              reject(new Error('来源页面已关闭'));
            }
          };
          const proposal = {
            text: value,
            previous: current.text,
            active,
            resolve,
            reject,
            cleanup: () => signal.removeEventListener('abort', cancel),
          };
          signal.addEventListener('abort', cancel, { once: true });
          pending.current = proposal;
          setProposal(proposal);
        });
      },
    };
    root.app_composer_handler = handler;
    return () => {
      if (root.app_composer_handler === handler) root.app_composer_handler = null;
      pending.current?.cleanup();
      pending.current?.reject(new Error('输入位置已变化'));
      pending.current = null;
    };
  }, [root, resources, scope]);
  function finish(replace: boolean) {
    const current = pending.current;
    pending.current = null;
    setProposal(null);
    if (!current) return;
    current.cleanup();
    if (!replace || !current.active() || latest.current.text !== current.previous) {
      current.reject(new Error('未替换草稿'));
      return;
    }
    latest.current.on_fill(current.text);
    current.resolve();
  }
  return (
    proposal && (
      <Dialog
        aria_label="替换输入草稿"
        backdrop_class_name={styles.backdrop}
        dialog_class_name={styles.dialog}
        on_close={() => finish(false)}
      >
        <h2>替换输入草稿？</h2>
        <p>当前草稿已有内容。替换后仍需你点击发送。</p>
        <div className={styles.actions}>
          <Button variant="text" onClick={() => finish(false)}>
            取消
          </Button>
          <Button onClick={() => finish(true)}>替换草稿</Button>
        </div>
      </Dialog>
    )
  );
});
