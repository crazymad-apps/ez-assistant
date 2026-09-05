import { useId, type CSSProperties, type ReactNode } from "react";
import { Collapse } from "../../../components/Collapse";
import { InlineIconButton } from "../../../components/InlineIconButton";
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
        <InlineIconButton
          aria-controls={content_id}
          aria-expanded={is_open}
          icon={is_open ? "chevron-up" : "chevron-down"}
          label={`${is_open ? "收起" : "展开"}${title}`}
          onClick={on_toggle}
        />
      </div>
      <Collapse id={content_id} open={is_open}>
        <div className={styles.section_body}>{children}</div>
      </Collapse>
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
