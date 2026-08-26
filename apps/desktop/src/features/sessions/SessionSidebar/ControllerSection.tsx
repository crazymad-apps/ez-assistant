import { observer } from "mobx-react-lite";
import type { ApplicationSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const ControllerSection = observer(function ControllerSection({
  application,
}: Readonly<{ application: ApplicationSnapshot | null }>) {
  const store = useRootStore();
  if (!application) {
    return <div aria-label="正在加载主控会话" className={styles.controller_skeleton} />;
  }
  const availability = application.controller_availability;
  if (availability.status === "unavailable") {
    return (
      <section aria-label="主控会话" className={styles.controller_section}>
        <div className={styles.controller_heading}>主控</div>
        <div className={styles.controller_unavailable}>主控会话暂不可用</div>
      </section>
    );
  }
  const session = application.active_sessions.find(
    (candidate) => candidate.session_id === availability.session_id,
  );
  if (!session) {
    return <div aria-label="正在加载主控会话" className={styles.controller_skeleton} />;
  }
  const status = session.active_compaction
    ? "压缩中"
    : session.pending_approval_count > 0
      ? "等待审批"
      : session.active_run_id
        ? "运行中"
        : session.queued_input_count > 0
          ? "排队"
          : "空闲";
  return (
    <section aria-label="主控会话" className={styles.controller_section}>
      <div className={styles.controller_heading}>主控</div>
      <button
        aria-current={store.navigation.selected_session_id === session.session_id ? "page" : undefined}
        className={styles.controller_row}
        onClick={() => void store.selectSession(session.session_id)}
        type="button"
      >
        <Icon name="bot" size={16} />
        <span>{session.title}</span>
        <small>{status}</small>
      </button>
    </section>
  );
});
