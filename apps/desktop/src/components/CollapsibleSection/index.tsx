import { useId, useState, type ReactNode } from "react";
import { Collapse } from "../Collapse";
import { Icon } from "../Icon";
import styles from "./index.module.scss";

/** 触发头与内容共享展开状态、键盘语义和既有 Collapse 生命周期。 */
export function CollapsibleSection(props: Readonly<{
  title: string;
  children: ReactNode;
  defaultOpen?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  actions?: ReactNode;
  variant?: "plain" | "outlined";
}>) {
  const [internalOpen, setInternalOpen] = useState(props.defaultOpen ?? false);
  const open = props.open ?? internalOpen;
  const content_id = useId();
  function toggle() {
    const next = !open;
    if (props.open === undefined) setInternalOpen(next);
    props.onOpenChange?.(next);
  }
  return <section className={styles.section} data-variant={props.variant ?? "plain"} data-open={open}>
    <header className={styles.header}>
      <button type="button" className={styles.trigger} aria-expanded={open} aria-controls={content_id} onClick={toggle}>
        <Icon name={open ? "chevron-down" : "chevron-right"} size={14} /><span>{props.title}</span>
      </button>
      {props.actions && <div className={styles.actions}>{props.actions}</div>}
    </header>
    <Collapse id={content_id} open={open}><div className={styles.body}>{props.children}</div></Collapse>
  </section>;
}
