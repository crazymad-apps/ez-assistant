import { observer } from "mobx-react-lite";
import type { QueueSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const QueueDrawer = observer(function QueueDrawer(props: Readonly<{
  open: boolean;
  on_open_change: (open: boolean) => void;
  queue: QueueSnapshot;
  session_id: string;
}>) {
  const store = useRootStore();
  const needs_resume = props.queue.state !== "automatic";
  return (
    <section className={styles.queue_drawer} data-open={props.open}>
      <button className={styles.queue_header} onClick={() => props.on_open_change(!props.open)} type="button">
        <Icon name="fork" size={16} />
        <strong>{needs_resume ? "待恢复队列" : "待执行队列"}</strong>
        <b>{props.queue.items.length}</b>
        <Icon name="chevron-down" size={14} />
      </button>
      {props.open && (
        <div className={styles.queue_items}>
          {props.queue.items.map((item) => (
            <div className={styles.queue_item} key={item.input_id}>
              <span>{item.position}</span>
              <p title={item.text_preview}>{item.text_preview}</p>
              <time>{formatTime(item.submitted_at_ms)}</time>
              <button
                disabled={store.pending_queue_input_id !== null}
                onClick={() => needs_resume
                  ? void store.resumeQueuedInput(props.session_id, item.input_id, props.queue.revision)
                  : void store.prioritizeQueuedInput(props.session_id, item.input_id, props.queue.revision)}
                type="button"
              >
                {needs_resume ? "恢复" : item.is_prioritized ? "已优先" : "优先"}
              </button>
              <button
                aria-label="移除排队输入"
                disabled={store.pending_queue_input_id !== null}
                onClick={() => void store.cancelQueuedInput(props.session_id, item.input_id)}
                type="button"
              >
                <Icon name="x" size={14} />
              </button>
            </div>
          ))}
        </div>
      )}
    </section>
  );
});

function formatTime(value: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(value);
}
