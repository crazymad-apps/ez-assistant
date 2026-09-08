import { observer } from "mobx-react-lite";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const SettingsMessages = observer(function SettingsMessages(props: Readonly<{ messages?: Readonly<{ error_message: string | null; notice_message: string | null }> }>) {
  const fallback = useRootStore().settings;
  const settings = props.messages ?? fallback;
  return (
    <>
      {settings.error_message && <p className={styles.error_message} role="alert">{settings.error_message}</p>}
      {settings.notice_message && <p className={styles.notice_message} role="status">{settings.notice_message}</p>}
    </>
  );
});
