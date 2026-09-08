import type { ReactNode } from "react";
import { InlineIconButton } from "../../../components/InlineIconButton";
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
          <div className={styles.page_container_title}>
            {props.on_back ? (
              <InlineIconButton
                icon="chevron-left"
                label={props.back_label ?? "返回"}
                onClick={props.on_back}
              />
            ) : null}
            <h3>{props.title}</h3>
          </div>
          {props.description ? <p>{props.description}</p> : null}
        </div>
        {props.actions ? <div className={styles.page_container_actions}>{props.actions}</div> : null}
      </header>
      <div className={styles.page_container_content}>{props.children}</div>
    </section>
  );
}
