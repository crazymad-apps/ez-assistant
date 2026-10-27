import { childKey } from "../../../../stores/liveExecutionProjection";
import { observer } from "mobx-react-lite";
import { useAppearance } from "../../../../stores/AppearanceContext";
import { useId, useLayoutEffect, useRef } from 'react';
import { Collapse } from '../../../../components/Collapse';
import { MarkdownContent } from '../../../../components/MarkdownContent';
import { useOutputFollow } from '../useOutputFollow';
import { useRootStore } from '../../../../stores/RootStoreContext';
import styles from './index.module.scss';

export const ReasoningBox = observer(function ReasoningBox(
  props: Readonly<{ state_key: string; text: string; summary?: boolean; is_open: boolean; on_toggle: () => void }>,
) {
  const theme = useAppearance().preset;
  const id = useId();
  const preview = useRef<HTMLElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const store = useRootStore();
  const session = store.navigation.selected_session_id;
  const child = store.navigation.selected_child_task_id;
  let history = session ? store.projection.conversation_histories.get(session) : undefined;
  if (child && session) history = store.projection.child_conversation_histories.get(childKey(session, child));
  const generation = history?.generation ?? 0;
  const open = !props.summary && props.is_open;
  useOutputFollow({
    scroll,
    content,
    identity: `${session}:${child}:${store.navigation.conversation_history_index}:${generation}:${props.state_key}`,
    active: open,
    restore: () => {
      const location = store.navigation.current_conversation_location;
      return location?.session_id === session &&
        location.child_task_id === child &&
        location.reading_generation === generation
        ? location.reasoning?.[props.state_key]
        : undefined;
    },
    save: (position) => {
      // 导航切换后的卸载回调不得把旧 reasoning 位置写入新的 owner。
      const location = store.navigation.current_conversation_location;
      if (location?.session_id === session && location.child_task_id === child) {
        store.navigation.updateReadingPosition(position, generation, props.state_key);
      }
    },
  });
  useLayoutEffect(() => {
    // 仅移动单行预览自身，不调用 scrollIntoView，避免抢走主列表和原文的自由滚动。
    const node = preview.current;
    if (!node) return;
    const followLatest = () => {
      node.scrollLeft = node.scrollWidth;
    };
    followLatest();
    const observer = new ResizeObserver(followLatest);
    observer.observe(node);
    return () => observer.disconnect();
  }, [props.text, props.summary, open]);
  if (props.summary) {
    return (
      <section className={styles.summary} aria-label="思考摘要" data-summary="true">
        <div className={styles.preview} ref={(node) => { preview.current = node; }}>
          <div className={styles.summary_text}>
            <MarkdownContent theme={theme} is_streaming text={props.text.replace(/\s+/g, ' ').trim()} />
          </div>
        </div>
      </section>
    );
  }
  return (
    <section className={styles.reasoning}>
      <button
        className={styles.header}
        aria-label="思考过程"
        aria-controls={id}
        aria-expanded={open}
        onClick={props.on_toggle}
        type="button"
      >
        <span>思考过程</span>
        {!open && (
          <span aria-hidden="true" className={styles.preview} ref={(node) => { preview.current = node; }}>
            {props.text.replace(/\s+/g, ' ').trim()}
          </span>
        )}
      </button>
      <Collapse open={open} id={id}>
        <div className={styles.content} ref={scroll} tabIndex={0} aria-label="思考内容" data-output-scroll="true">
          <div ref={content}>{props.text}</div>
        </div>
      </Collapse>
    </section>
  );
});
