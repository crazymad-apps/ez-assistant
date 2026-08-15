import type { ReactNode } from "react";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import styles from "./index.module.scss";

type SessionActionDialogProps = Readonly<{
  title: string;
  children: ReactNode;
  confirm_label: string;
  is_danger?: boolean;
  is_pending: boolean;
  on_cancel: () => void;
  on_confirm: () => void;
}>;

export function SessionActionDialog({
  title,
  children,
  confirm_label,
  is_danger = false,
  is_pending,
  on_cancel,
  on_confirm,
}: SessionActionDialogProps) {
  return (
    <Dialog
      aria_labelledby="session-action-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      dismissible={!is_pending}
      on_close={on_cancel}
    >
        <header className={styles.header}>
          <h2 id="session-action-title">{title}</h2>
          <button aria-label="关闭" disabled={is_pending} onClick={on_cancel} type="button">
            <Icon name="x" size={18} />
          </button>
        </header>
        <div className={styles.body}>{children}</div>
        <footer className={styles.footer}>
          <button disabled={is_pending} onClick={on_cancel} type="button">取消</button>
          <button
            className={is_danger ? styles.danger : styles.primary}
            disabled={is_pending}
            onClick={on_confirm}
            type="button"
          >
            {is_pending ? "正在处理…" : confirm_label}
          </button>
        </footer>
    </Dialog>
  );
}
