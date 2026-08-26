import { observer } from "mobx-react-lite";
import { useRef } from "react";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import type { ConversationItem } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { MarkdownContent } from "../../../components/MarkdownContent";
import styles from "./index.module.scss";

/** 历史命中的只读上下文预览；预览本身不会写入当前 Agent 上下文。 */
export const ConversationSearchDialog = observer(function ConversationSearchDialog() {
  const store = useRootStore();
  const hit = store.conversation_search.selected_hit;
  const close_button_ref = useRef<HTMLButtonElement>(null);
  if (!hit) return null;

  const recall = store.conversation_search.recall_window;
  return (
    <Dialog
      aria_labelledby="conversation-search-dialog-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      initial_focus_ref={close_button_ref}
      on_close={() => store.conversation_search.closeRecall()}
    >
      <header>
        <div>
          <h2 id="conversation-search-dialog-title">{hit.child_task_title ?? hit.session_title}</h2>
          {hit.child_task_title && <p>{hit.session_title}</p>}
        </div>
        <button
          aria-label="关闭搜索结果"
          className={styles.close_button}
          onClick={() => store.conversation_search.closeRecall()}
          ref={close_button_ref}
          type="button"
        >
          <Icon name="x" size={17} />
        </button>
      </header>
      <div className={styles.body}>
        {store.conversation_search.recall_loading && <p className={styles.state}>正在读取消息上下文…</p>}
        {store.conversation_search.recall_error && <p className={styles.error}>{store.conversation_search.recall_error}</p>}
        {recall?.items.map((item) => (
          <RecallMessage
            is_anchor={item.message_id === recall.anchor_message_id}
            item={item}
            key={item.message_id}
          />
        ))}
      </div>
      <footer>
        <span>只读预览，不会加入当前 Agent 上下文</span>
        <div>
          <button onClick={() => store.conversation_search.closeRecall()} type="button">关闭</button>
          <button
            className={styles.primary_button}
            disabled={!recall}
            onClick={() => void store.openConversationHistoryHit(hit)}
            type="button"
          >
            在原会话中查看
          </button>
        </div>
      </footer>
    </Dialog>
  );
});

function RecallMessage(props: Readonly<{ item: ConversationItem; is_anchor: boolean }>) {
  if (props.item.type === "user") {
    return (
      <article className={styles.message} data-anchor={props.is_anchor} data-role="user">
        <small>用户</small>
        <p>{props.item.text}</p>
      </article>
    );
  }
  if (props.item.type === "context_summary") {
    return (
      <article className={styles.message} data-anchor={props.is_anchor} data-role="summary">
        <small>上下文摘要</small>
        <MarkdownContent text={props.item.text} />
      </article>
    );
  }
  const text = props.item.segments
    .filter((segment) => segment.type === "text")
    .map((segment) => segment.text)
    .join("\n\n");
  return (
    <article className={styles.message} data-anchor={props.is_anchor} data-role="assistant">
      <small>助手</small>
      {text ? <MarkdownContent text={text} /> : <p className={styles.no_text}>此消息仅包含工具调用或思考过程。</p>}
    </article>
  );
}
