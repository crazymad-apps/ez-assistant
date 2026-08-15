import { useId, type CSSProperties, type ReactNode } from "react";
import { Icon } from "../../../components/Icon";
import styles from "./index.module.scss";

export function ContextSection({ children, is_open, on_toggle, title }: Readonly<{
  children: ReactNode;
  is_open: boolean;
  on_toggle: () => void;
  title: string;
}>) {
  const content_id = useId();
  return (
    <section className={styles.context_section}>
      <button
        aria-controls={content_id}
        aria-expanded={is_open}
        className={styles.section_heading}
        onClick={on_toggle}
        type="button"
      >
        <span>{title}</span>
        <Icon name={is_open ? "chevron-up" : "chevron-down"} size={14} />
      </button>
      {is_open && <div className={styles.section_body} id={content_id}>{children}</div>}
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
