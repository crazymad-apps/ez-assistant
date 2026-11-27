import { isTauri } from "@tauri-apps/api/core";
import { useRef, type ReactNode } from "react";
import appLogo from "../../../../app-icon.svg";
import {
  gsap,
  MOTION_DURATION,
  MOTION_EASE,
  runMotion,
  useGSAP,
} from "../../../components/Motion";
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
  const page_ref = useRef<HTMLElement>(null);

  useGSAP(
    () => runMotion((reduced) => {
      if (reduced) return;
      const timeline = gsap.timeline({ defaults: { ease: MOTION_EASE.emphasis } });
      timeline
        .from("[data-entry-brand]", {
          clearProps: "transform,opacity",
          duration: MOTION_DURATION.entrance,
          opacity: 0,
          scale: 0.95,
          y: 10,
        })
        .from(
          "[data-entry-body]",
          {
            clearProps: "transform,opacity",
            duration: MOTION_DURATION.entrance,
            opacity: 0,
            y: 12,
          },
          "<0.08",
        );
    }, page_ref),
    { scope: page_ref },
  );

  return (
    <main className={styles.page} data-runtime-entry ref={page_ref}>
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
        <div className={styles.brand} data-entry-brand><img className={styles.mark} src={appLogo} alt="" /><span>ez-assistant</span></div>
        <div data-entry-body>{children}</div>
      </section>
    </main>
  );
}
