import { createRoot } from "react-dom/client";
import { AppearanceStore } from "../../../src/stores/AppearanceStore";
import { AppearanceProvider } from "../../../src/stores/AppearanceContext";
import { RootStore } from "../../../src/stores/RootStore";
import { RootStoreProvider } from "../../../src/stores/RootStoreContext";
import { AppMessageProvider } from "../../../src/components/AppMessage";
import { AppShell } from "../../../src/app/AppShell";
import { SettingsDialog } from "../../../src/features/settings/SettingsDialog";
import { DesktopEntryPage } from "../../../src/features/runtime-access/DesktopEntryPage";
import { ApplicationConnectionStore } from "../../../src/features/runtime-access/ApplicationConnectionStore";
import { applyDocumentTheme, isThemeId } from "../../../src/styles/themePresets";
import "../../../src/styles/index.scss";

// 实际设置组件的浏览器布局夹具；内存偏好，不连接 Host 或访问用户存储。
const appearance = new AppearanceStore({ load: () => ({ theme_id: "default_light", recovered_unknown: false }), save: () => undefined }, { apply: applyDocumentTheme, applyNative: async () => undefined });
await appearance.initialize();
const params = new URLSearchParams(location.search);
const theme = params.get("theme");
if (isThemeId(theme)) appearance.selectTheme(theme);
const root = new RootStore();
const connection = new ApplicationConnectionStore(root);
root.settings.is_open = true;
root.settings.page = "appearance";
createRoot(document.getElementById("app")!).render(<AppearanceProvider store={appearance}><RootStoreProvider store={root}><AppMessageProvider>{params.has("entry") ? <DesktopEntryPage connection={connection} /> : params.has("shell") ? <AppShell /> : <SettingsDialog />}</AppMessageProvider></RootStoreProvider></AppearanceProvider>);
window.addEventListener("pagehide", () => { connection.dispose(); root.dispose(); appearance.dispose(); }, { once: true });
