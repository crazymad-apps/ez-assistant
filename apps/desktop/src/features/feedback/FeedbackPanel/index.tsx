import { useEffect, useId, useRef } from 'react';
import { observer } from 'mobx-react-lite';
import type { FeedbackRequestSnapshot } from '@ez-assistant/protocol';
import { Icon } from '../../../components/Icon';
import { Button } from '../../../components/Button';
import { useInputMethodGuard } from '../../../components/InputMethodGuard';
import { answerComplete, type FeedbackStore } from '../FeedbackStore';
import styles from './index.module.scss';

type FeedbackPanelProps = Readonly<{ request: FeedbackRequestSnapshot; store: FeedbackStore }>;

/** 题目导航只改变本地草稿位置；只有最后一步显式提交才恢复 Agent 执行。 */
export const FeedbackPanel = observer(function FeedbackPanel(props: FeedbackPanelProps) {
  const { request, store } = props;
  const draft = store.draft(request);
  const operation = store.pending.get(request.request_id);
  const busy = operation !== undefined;
  const question = request.questions[draft.active];
  const answer = draft.answers[draft.active];
  const complete = draft.answers.every(answerComplete);
  const current_complete = answerComplete(answer);
  const last = draft.active === request.questions.length - 1;
  const has_options = question.options.length > 0;
  const has_selection = answer.selected_options.length > 0;
  const input_label = has_selection ? '补充说明（可选）' : '你的回答';
  const input_id = useId();
  const input_method = useInputMethodGuard();
  const fields = useRef<HTMLFieldSetElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const submit_label = request.mode === 'wait' ? '回复并继续' : '发送回复';
  let primary_label = submit_label;
  if (!last) primary_label = '下一题';
  else if (!complete && request.questions.length > 1) primary_label = '前往未答题';
  if (operation === 'answer') primary_label = '提交中…';

  useEffect(() => {
    body.current?.scrollTo?.({ top: 0 });
    // 导航后聚焦当前题，不选择答案；输入过程中不因进度变化抢走焦点。
    const field = fields.current;
    const target = field?.querySelector<HTMLInputElement>('input:checked')
      ?? field?.querySelector<HTMLInputElement | HTMLTextAreaElement>('input, textarea');
    target?.focus({ preventScroll: true });
  }, [draft.active, store.focus_id, store.focus_revision, request.request_id]);

  if (request.status !== 'pending' && !busy) return null;

  function setAnswer(update: Partial<typeof answer>) {
    store.setDraft(request, {
      answers: draft.answers.map((item, index) => index === draft.active ? { ...item, ...update } : item),
    });
  }

  function goTo(index: number) {
    if (!busy) store.setDraft(request, { active: index });
  }

  function advance() {
    if (busy || !current_complete) return;
    if (!last) {
      goTo(draft.active + 1);
      return;
    }
    if (!complete) {
      goTo(draft.answers.findIndex((item) => !answerComplete(item)));
      return;
    }
    void store.answer(request);
  }

  function selectOption(index: number, checked: boolean) {
    let selected_options = [index];
    if (question.multiple) {
      selected_options = answer.selected_options.filter((value) => value !== index);
      if (checked) selected_options = [...selected_options, index].sort((a, b) => a - b);
    }
    setAnswer({ selected_options });
  }

  return (
    <section aria-busy={busy} aria-label="等待你回答" className={styles.panel}
      onKeyDown={(event) => {
        if (event.key !== 'Enter' || !(event.ctrlKey || event.metaKey) || event.altKey
          || event.repeat || input_method.shouldIgnoreKeyDown(event)) return;
        event.preventDefault();
        advance();
      }}>
      <header className={styles.header}>
        <span className={styles.header_icon}><Icon name="message" size={17} /></span>
        {request.questions.length === 1 ? <span>等待你的回复</span> : (
          <nav aria-label="问题切换" className={styles.tabs}>
            {request.questions.map((item, index) => (
              <Button aria-current={index === draft.active ? 'step' : undefined}
                aria-label={item.header ?? `问题 ${index + 1}`} key={index} disabled={busy}
                onClick={() => goTo(index)} size="small" variant="text">
                <span className={styles.step} data-complete={answerComplete(draft.answers[index])} aria-hidden="true">
                  {answerComplete(draft.answers[index]) ? <Icon name="check" size={12} /> : index + 1}
                </span>
                <span>{item.header ?? `问题 ${index + 1}`}</span>
              </Button>
            ))}
          </nav>
        )}
      </header>
      <div className={styles.body} ref={body}>
        <fieldset className={styles.question} disabled={busy} ref={fields}>
          <legend>{question.question}</legend>
          {has_options && <div className={styles.option_header}>
            <span>{question.multiple ? '可多选，也可填写答案' : '选择一项，或填写答案'}</span>
            <Button aria-label="清除选择" className={styles.clear} disabled={busy || !has_selection}
              onClick={() => setAnswer({ selected_options: [] })} size="small" variant="text">清除</Button>
          </div>}
          {has_options && <div className={styles.options}>
            {question.options.map((option, index) => (
              <label className={styles.option} data-selected={answer.selected_options.includes(index)} key={option.label}>
                <input type={question.multiple ? 'checkbox' : 'radio'}
                  name={`feedback-${request.request_id}-${draft.active}`} checked={answer.selected_options.includes(index)}
                  onChange={(event) => selectOption(index, event.target.checked)} />
                <span><span>{option.label}</span>{option.description && <small>{option.description}</small>}</span>
              </label>
            ))}
          </div>}
          <div className={styles.answer}>
            {has_options && <label htmlFor={input_id}>{input_label}</label>}
            <textarea id={input_id} aria-label={input_label}
              placeholder={has_selection ? '需要补充的细节…' : '输入你的答案…'}
              value={answer.text} onChange={(event) => setAnswer({ text: event.target.value })} rows={2}
              onCompositionStart={input_method.onCompositionStart} onCompositionEnd={input_method.onCompositionEnd}
              onKeyUp={input_method.onKeyUp} />
          </div>
        </fieldset>
        {store.errors.get(request.request_id) && <p className={styles.error} role="alert">
          {store.errors.get(request.request_id)}
        </p>}
      </div>
      <footer className={styles.actions}>
        <Button disabled={busy} onClick={() => void store.cancel(request)} variant="text">
          {operation === 'cancel' ? '取消中…' : '取消'}
        </Button>
        <div className={styles.navigation}>
          {draft.active > 0 && <Button disabled={busy} onClick={() => goTo(draft.active - 1)} variant="text">上一题</Button>}
          <Button aria-keyshortcuts="Control+Enter Meta+Enter" disabled={busy || !current_complete}
            onClick={advance} variant="primary">{primary_label}</Button>
        </div>
      </footer>
    </section>
  );
});
