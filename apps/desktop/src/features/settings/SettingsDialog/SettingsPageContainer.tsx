import type { ReactNode } from "react";
import { Icon } from "../../../components/Icon";
import styles from "./index.module.scss";

type SettingsPageContainerProps = Readonly<{
  actions?: ReactNode;
  back_label?: string;
  children: ReactNode;
  description?: ReactNode;
  on_back?: () => void;
  title: ReactNode;
}>;

/** 设置一级页和二级页共用的固定页头与单一内容滚动容器。 */
export function SettingsPageContainer(props: SettingsPageContainerProps) {
  return (
    <section className={styles.page_container}>
      <header className={styles.page_container_header}>
        <div className={styles.page_container_heading}>
          {props.on_back ? (
            <button
              aria-label={props.back_label ?? "返回"}
              className={styles.page_container_back}
              onClick={props.on_back}
              type="button"
            >
              <Icon name="chevron-left" size={16} />
            </button>
          ) : null}
          <div>
            <h3>{props.title}</h3>
            {props.description ? <p>{props.description}</p> : null}
          </div>
        </div>
        {props.actions ? <div className={styles.page_container_actions}>{props.actions}</div> : null}
      </header>
      <div className={styles.page_container_content}>{props.children}</div>
    </section>
  );
}
