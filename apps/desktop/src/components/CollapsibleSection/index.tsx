import { useId, useState, type ReactNode } from "react";
import { Collapse } from "../Collapse";
import { Icon } from "../Icon";
import styles from "./index.module.scss";

/** 触发头与内容共享展开状态、键盘语义和既有 Collapse 生命周期。 */
export function CollapsibleSection(props: Readonly<{
  title: string; children: ReactNode; defaultOpen?: boolean;
}>) {
  const [open, setOpen] = useState(props.defaultOpen ?? false);
  const content_id = useId();
  return <section className={styles.section}>
    <button type="button" className={styles.trigger} aria-expanded={open} aria-controls={content_id} onClick={() => setOpen(!open)}>
      <Icon name={open ? "chevron-down" : "chevron-right"} size={14} /><span>{props.title}</span>
    </button>
    <Collapse id={content_id} open={open}><div className={styles.body}>{props.children}</div></Collapse>
  </section>;
}
