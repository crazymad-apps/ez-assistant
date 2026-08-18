import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import type { SettingsPage } from "../../../stores/SettingsStore";
import { ModelsSettingsPage } from "./ModelsSettingsPage";
import { MemorySettingsPage } from "./MemorySettingsPage";
import { PermissionSettingsPage } from "./PermissionSettingsPage";
import { RuntimeSettingsPage } from "./RuntimeSettingsPage";
import styles from "./index.module.scss";

const pages: ReadonlyArray<{ id: SettingsPage; label: string; icon: "terminal" | "bot" | "shield" | "pin" }> = [
  { id: "runtime", label: "Runtime", icon: "terminal" },
  { id: "models", label: "模型", icon: "bot" },
  { id: "memory", label: "记忆", icon: "pin" },
  { id: "permissions", label: "权限", icon: "shield" },
];

export const SettingsDialog = observer(function SettingsDialog() {
  const store = useRootStore();
  const settings = store.settings;
  const close_button_ref = useRef<HTMLButtonElement>(null);
  const [form_dirty, setFormDirty] = useState(false);

  useEffect(() => {
    if (settings.is_open && settings.page === "memory") {
      void store.memory_settings.load();
    }
  }, [settings.is_open, settings.page, store.memory_settings]);

  if (!settings.is_open) return null;

  function requestClose() {
    if (form_dirty && !window.confirm("当前表单尚未保存，确定关闭设置吗？")) return;
    setFormDirty(false);
    settings.close();
  }

  function selectPage(page: SettingsPage) {
    if (form_dirty && !window.confirm("当前表单尚未保存，确定切换页面吗？")) return;
    setFormDirty(false);
    settings.selectPage(page);
  }

  return (
    <Dialog
      aria_label="设置"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      initial_focus_ref={close_button_ref}
      on_close={requestClose}
    >
        <header className={styles.dialog_header}>
          <h2>设置</h2>
          <button aria-label="关闭设置" onClick={requestClose} ref={close_button_ref} type="button">
            <Icon name="x" size={17} />
          </button>
        </header>
        <div className={styles.dialog_body}>
          <nav aria-label="设置页面" className={styles.navigation}>
            {pages.map((page) => (
              <button
                aria-current={settings.page === page.id ? "page" : undefined}
                key={page.id}
                onClick={() => selectPage(page.id)}
                type="button"
              >
                <Icon name={page.icon} size={16} />
                {page.label}
              </button>
            ))}
          </nav>
          <div className={styles.content}>
            {settings.page === "runtime" && <RuntimeSettingsPage />}
            {settings.page === "models" && (
              <ModelsSettingsPage onDirtyChange={setFormDirty} />
            )}
            {settings.page === "permissions" && (
              <PermissionSettingsPage onDirtyChange={setFormDirty} />
            )}
            {settings.page === "memory" && (
              <MemorySettingsPage onDirtyChange={setFormDirty} />
            )}
          </div>
        </div>
    </Dialog>
  );
});
