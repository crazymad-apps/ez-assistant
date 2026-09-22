import { isTauri } from "@tauri-apps/api/core";
import type { ReactNode } from "react";
import appLogo from "../../../../app-icon.svg";
import { DotsBackground } from "./DotsBackground";
import styles from "./index.module.scss";
import { DesktopWindowControls } from "../../desktop-lifecycle/DesktopWindowControls";

type RuntimeEntryLayoutProps = {
  readonly children: ReactNode;
};

/** Desktop 连接与 Web 登录共用视觉外壳；表单和连接状态由各自 children 承载。 */
export function RuntimeEntryLayout({
  children,
}: RuntimeEntryLayoutProps) {
  return (
    <main className={styles.page} data-runtime-entry>
      <header
        className={styles.title_bar}
        data-native-window={isTauri()}
        data-tauri-drag-region
      >
        <span data-tauri-drag-region>ez-assistant</span>
        <small data-tauri-drag-region>v{__APP_VERSION__}</small>
        <DesktopWindowControls />
      </header>
      <DotsBackground />
      <section className={styles.content} data-runtime-entry-content>
        <img className={styles.mark} src={appLogo} alt="EZ Assistant" />
        {children}
      </section>
    </main>
  );
}
