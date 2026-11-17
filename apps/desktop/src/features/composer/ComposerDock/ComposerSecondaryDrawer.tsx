import type { ReactNode } from "react";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import { useId } from "react";
import styles from "./index.module.scss";

type ComposerSecondaryDrawerProps = Readonly<{
  actions?: ReactNode;
  children?: ReactNode;
  label: string;
  on_open_change?: (open: boolean) => void;
  open?: boolean;
  state?: string;
  summary: ReactNode;
}>;

/**
 * Composer 中占据正常文档流的二级抽屉外壳。
 * Goal 与 Queue 共享 header、展开间距和圆角接缝处理，业务组件只组合摘要、动作与正文。
 */
export function ComposerSecondaryDrawer(props: ComposerSecondaryDrawerProps) {
  const content_id = useId();
  const toggle = props.on_open_change;
  return (
    <section
      aria-label={props.label}
      className={styles.secondary_drawer}
      data-open={props.open}
      data-state={props.state}
      data-title-only={toggle ? undefined : true}
    >
      <header className={styles.secondary_drawer_header}>
        {toggle ? <button
          aria-controls={content_id}
          aria-expanded={props.open}
          className={styles.secondary_drawer_trigger}
          onClick={() => toggle(!props.open)}
          type="button"
        >
          {props.summary}
          <Icon name="chevron-down" size={14} />
        </button> : <div className={styles.secondary_drawer_title}>{props.summary}</div>}
        {props.actions && <div className={styles.secondary_drawer_actions}>{props.actions}</div>}
      </header>
      {toggle && <Collapse id={content_id} open={Boolean(props.open)}>{props.children}</Collapse>}
    </section>
  );
}
