import { isTauri } from "@tauri-apps/api/core";
import type { ReactNode } from "react";
import appLogo from "../../../../app-icon.svg";
import { DotsBackground } from "./DotsBackground";
import styles from "./index.module.scss";

type RuntimeEntryLayoutProps = {
  readonly description: string;
  readonly children: ReactNode;
};

/** Desktop 连接与 Web 登录共用视觉外壳；表单和连接状态由各自 children 承载。 */
export function RuntimeEntryLayout({
  description,
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
      </header>
      <DotsBackground />
      <section className={styles.content} data-runtime-entry-content>
        <img className={styles.mark} src={appLogo} alt="EZ Assistant" />
        <h1>连接你的工作空间</h1>
        <p className={styles.subtitle}>{description}</p>
        {children}
      </section>
      <footer className={styles.footer}>你的工作，由此连接。</footer>
    </main>
  );
}
