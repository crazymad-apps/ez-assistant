import { observer } from "mobx-react-lite";
import { useEffect } from "react";
import { useAppMessage } from "../../../components/AppMessage";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const SettingsMessages = observer(function SettingsMessages(props: Readonly<{ messages?: Readonly<{ error_message: string | null; notice_message: string | null; dismissNotice: (message: string) => void }> }>) {
  const fallback = useRootStore().settings;
  const settings = props.messages ?? fallback;
  const show_message = useAppMessage();
  const notice = settings.notice_message;
  useEffect(() => {
    if (!notice) return;
    show_message(notice);
    settings.dismissNotice(notice);
  }, [notice, settings, show_message]);
  return (
    <>
      {settings.error_message && <p className={styles.error_message} role="alert">{settings.error_message}</p>}
    </>
  );
});
