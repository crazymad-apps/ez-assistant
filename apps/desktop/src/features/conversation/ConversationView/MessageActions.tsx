import { useEffect, useRef, useState, type ReactNode } from "react";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import { copyText } from "../../../platform/clipboard";
import styles from "./index.module.scss";

export function MessageActions(props: Readonly<{
  text: string;
  copy_label: string;
  time?: string;
  children?: ReactNode;
}>) {
  const [notice, setNotice] = useState("");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const active = useRef(true);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
      if (timer.current !== null) clearTimeout(timer.current);
    };
  }, []);

  async function copy() {
    let message = "这条消息没有可复制的文本";
    if (props.text.trim()) {
      try {
        await copyText(props.text);
        message = "已复制";
      } catch {
        message = "复制失败，请重试";
      }
    }
    if (!active.current) return;
    if (timer.current !== null) clearTimeout(timer.current);
    setNotice(message);
    timer.current = setTimeout(() => setNotice(""), 1800);
  }

  return (
    <div className={styles.message_actions}>
      <Tooltip content={props.copy_label}>
        <button aria-label={props.copy_label} onClick={() => void copy()} type="button">
          <Icon name="copy" size={15} />
        </button>
      </Tooltip>
      {props.children}
      {props.time !== undefined && <time>{props.time}</time>}
      {notice && <span className={styles.message_action_notice} role="status">{notice}</span>}
    </div>
  );
}
