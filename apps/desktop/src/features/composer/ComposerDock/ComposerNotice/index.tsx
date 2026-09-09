import type { ReactNode } from "react";
import { Button } from "../../../../components/Button";
import { Icon } from "../../../../components/Icon";
import styles from "./index.module.scss";

type ComposerNoticeProps = Readonly<{
  children: ReactNode;
  tone: "error" | "success" | "warning" | "neutral";
  action?: Readonly<{ label: string; onClick: () => void }>;
  dismiss?: Readonly<{ label: string; onClick: () => void }>;
}>;

export function ComposerNotice(props: ComposerNoticeProps) {
  return (
    <div className={styles.notice} data-tone={props.tone} role={props.tone === "error" ? "alert" : "status"}>
      <div className={styles.message}>{props.children}</div>
      {(props.action || props.dismiss) && (
        <div className={styles.actions}>
          {props.action && <Button onClick={props.action.onClick} size="small" variant="text">{props.action.label}</Button>}
          {props.dismiss && (
            <Button aria-label={props.dismiss.label} iconOnly onClick={props.dismiss.onClick} size="small" variant="text">
              <Icon name="x" size={14} />
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
