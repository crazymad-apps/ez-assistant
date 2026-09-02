import { useId, type CSSProperties, type ReactNode } from "react";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import styles from "./index.module.scss";

export function ContextSection({ action, children, is_open, on_toggle, title }: Readonly<{
  action?: ReactNode;
  children: ReactNode;
  is_open: boolean;
  on_toggle: () => void;
  title: string;
}>) {
  const content_id = useId();
  return (
    <section className={styles.context_section}>
      <div className={styles.section_heading_row}>
        <button
          aria-controls={content_id}
          aria-expanded={is_open}
          className={styles.section_heading}
          onClick={on_toggle}
          type="button"
        >
          <span>{title}</span>
        </button>
        {action}
        <button
          aria-controls={content_id}
          aria-expanded={is_open}
          aria-label={`${is_open ? "收起" : "展开"}${title}`}
          className={styles.section_toggle}
          onClick={on_toggle}
          type="button"
        >
          <Icon name={is_open ? "chevron-up" : "chevron-down"} size={14} />
        </button>
      </div>
      <Collapse class_name={styles.section_body} id={content_id} open={is_open}>{children}</Collapse>
    </section>
  );
}

export function ContextRing({ basis_points }: Readonly<{ basis_points: number }>) {
  const degrees = Math.min(360, Math.max(0, basis_points * 0.036));
  return (
    <span
      aria-hidden="true"
      className={styles.context_ring}
      style={{ "--context-degrees": `${degrees}deg` } as CSSProperties}
    />
  );
}
