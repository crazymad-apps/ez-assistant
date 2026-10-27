import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import { Dialog } from "../../../components/Dialog";
import { Icon, type IconName } from "../../../components/Icon";
import { PresenceBoundary } from "../../../components/Presence";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { useRootStore } from "../../../stores/RootStoreContext";
import type { SettingsPage } from "../../../stores/SettingsStore";
import { ModelsSettingsPage } from "./ModelsSettingsPage";
import { MemorySettingsPage } from "./MemorySettingsPage";
import { PermissionSettingsPage } from "./PermissionSettingsPage";
import { RuntimeSettingsPage } from "./RuntimeSettingsPage";
import { SkillSettingsPage } from "./SkillSettingsPage";
import { DeviceSettingsPage } from "./DeviceSettingsPage";
import { McpSettingsPage } from "./McpSettingsPage";
import { AgentSettingsPage } from "./AgentSettingsPage";
import { PluginSettingsPage } from "./PluginSettingsPage";
import { NotificationSettingsPage } from "./NotificationSettingsPage";
import { AppearanceSettingsPage } from "./AppearanceSettingsPage";
import styles from "./index.module.scss";

const pages: ReadonlyArray<{ id: SettingsPage; label: string; icon: IconName }> = [
  { id: "appearance", label: "外观", icon: "palette" },
  { id: "notifications", label: "系统通知", icon: "bell" },
  { id: "runtime", label: "Runtime", icon: "terminal" },
  { id: "devices", label: "智能终端", icon: "device" },
  { id: "models", label: "模型", icon: "model" },
  { id: "memory", label: "记忆", icon: "pin" },
  { id: "permissions", label: "权限", icon: "shield" },
  { id: "skills", label: "技能", icon: "folder" },
  { id: "mcp", label: "MCP", icon: "puzzle" },
  { id: "plugins", label: "插件", icon: "plugin" },
  { id: "agents", label: "智能体", icon: "bot" },
];

const runtime_pages: readonly SettingsPage[] = ["runtime", "runtime_connection", "host_access", "runtime_diagnostics", "runtime_local", "host_registration"];

export const SettingsDialog = observer(function SettingsDialog() {
  const store = useRootStore();
  const settings = store.settings;
  const close_button_ref = useRef<HTMLButtonElement>(null);
  const [form_pending, setFormPending] = useState(false);
  const [form_dirty, setFormDirty] = useState(false);
  const [pending_navigation, setPendingNavigation] = useState<Readonly<
    { type: "close" } | { type: "page"; page: SettingsPage }
  > | null>(null);

  useEffect(() => {
    if (settings.is_open && settings.page === "memory") {
      void store.memory_settings.load();
    }
  }, [settings.is_open, settings.page, store.memory_settings]);

  function requestClose() {
    if (form_pending) return;
    if (form_dirty) {
      setPendingNavigation({ type: "close" });
      return;
    }
    setFormDirty(false);
    settings.close();
  }

  function selectPage(page: SettingsPage) {
    if (form_pending) return;
    if (form_dirty) {
      setPendingNavigation({ type: "page", page });
      return;
    }
    setFormDirty(false);
    settings.selectPage(page);
  }

  function confirmNavigation() {
    const navigation = pending_navigation;
    if (!navigation) return;
    setPendingNavigation(null);
    setFormDirty(false);
    if (navigation.type === "close") settings.close();
    else settings.selectPage(navigation.page);
  }

  return (
    <>
    <Dialog
      aria_label="设置"
      dismissible={!form_pending}
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      initial_focus_ref={close_button_ref}
      on_close={requestClose}
      open={settings.is_open}
    >
        <header className={styles.dialog_header}>
          <h2>设置</h2>
          <button disabled={form_pending} aria-label="关闭设置" onClick={requestClose} ref={close_button_ref} type="button">
            <Icon name="x" size={17} />
          </button>
        </header>
        <div className={styles.dialog_body}>
          <nav aria-label="设置页面" className={styles.navigation}>
            {pages.map((page) => (
              <button
                aria-current={(page.id === "runtime" ? runtime_pages.includes(settings.page) : settings.page === page.id) ? "page" : undefined}
                disabled={form_pending}
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
            {settings.page === "appearance" && <AppearanceSettingsPage />}
            {runtime_pages.includes(settings.page) && <RuntimeSettingsPage onNavigate={selectPage} onDirtyChange={setFormDirty} onPendingChange={setFormPending} />}
            {settings.page === "notifications" && <NotificationSettingsPage />}
            {settings.page === "devices" && <DeviceSettingsPage />}
            {settings.page === "models" && (
              <ModelsSettingsPage onDirtyChange={setFormDirty} />
            )}
            {settings.page === "permissions" && (
              <PermissionSettingsPage onDirtyChange={setFormDirty} />
            )}
            {settings.page === "memory" && (
              <MemorySettingsPage onDirtyChange={setFormDirty} />
            )}
            {settings.page === "skills" && <SkillSettingsPage />}
            {settings.page === "mcp" && <McpSettingsPage onDirtyChange={setFormDirty} />}
            {settings.page === "plugins" && <PluginSettingsPage onDirtyChange={setFormDirty} />}
            {settings.page === "agents" && <AgentSettingsPage onDirtyChange={setFormDirty} />}
          </div>
        </div>
    </Dialog>
    <PresenceBoundary present={pending_navigation !== null}>
      {pending_navigation && (
        <SessionActionDialog
          confirm_label={pending_navigation.type === "close" ? "关闭设置" : "切换页面"}
          is_danger
          is_pending={false}
          on_cancel={() => setPendingNavigation(null)}
          on_confirm={confirmNavigation}
          title="放弃未保存的修改？"
        >
          <p>当前表单尚未保存，继续后这些修改将丢失。</p>
        </SessionActionDialog>
      )}
    </PresenceBoundary>
    </>
  );
});
