import { isTauri } from "@tauri-apps/api/core";
import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import appLogo from "../../../../../app-icon.svg";
import { Button } from "../../../../components/Button";
import { Icon } from "../../../../components/Icon";
import { getDesktopPlatform, type DesktopPlatform } from "../../../../native-bridge/desktopLifecycle";
import { copyText } from "../../../../platform/clipboard";
import { useRootStore } from "../../../../stores/RootStoreContext";
import type { SettingsPage } from "../../../../stores/SettingsStore";
import { useApplicationConnection } from "../../../runtime-access/ApplicationConnectionContext";
import { SettingsMessages } from "../SettingsMessages";
import { SettingsPageContainer } from "../SettingsPageContainer";
import {
  APP_CAPABILITIES,
  buildEnvironmentSummary,
  CURRENT_RELEASE_META,
  CURRENT_VERSION_FEATURES,
  LICENSE_ATTRIBUTIONS,
  RELEASE_CHANGELOG,
} from "./aboutContent";
import styles from "./index.module.scss";

type AboutSubView = "overview" | "changelog" | "features" | "licenses";

type AboutSettingsPageProps = Readonly<{
  onNavigate: (page: SettingsPage) => void;
}>;

/** 「设置 → 关于」一级概览与二级信息页，复用统一返回容器与设计 Token。 */
export const AboutSettingsPage = observer(function AboutSettingsPage(props: AboutSettingsPageProps) {
  const root = useRootStore();
  const app_connection = useApplicationConnection();
  const [view, setView] = useState<AboutSubView>("overview");
  const [copied, setCopied] = useState(false);
  const [desktop_platform, setDesktopPlatform] = useState<DesktopPlatform>("unsupported");
  const copy_timer_ref = useRef<number | null>(null);

  const is_desktop = isTauri();
  const connection = root.local_connection;
  const capabilities = connection.capabilities;
  const connected = connection.state === "connected";
  const has_feedback_center = Boolean(app_connection?.session?.identity);
  const has_update = Boolean(app_connection?.updates.visible);

  useEffect(() => {
    if (!is_desktop) return undefined;
    let active = true;
    void getDesktopPlatform()
      .then((platform) => {
        if (active) setDesktopPlatform(platform);
      })
      .catch(() => {
        /* 原生平台读取失败时省略系统字段，不阻断页面展示。 */
      });
    return () => {
      active = false;
    };
  }, [is_desktop]);

  useEffect(() => {
    return () => {
      if (copy_timer_ref.current !== null) {
        window.clearTimeout(copy_timer_ref.current);
        copy_timer_ref.current = null;
      }
    };
  }, []);

  async function handleCopyEnvironment() {
    const summary = buildEnvironmentSummary({
      app_version: __APP_VERSION__,
      client_kind: is_desktop ? "desktop" : "web",
      desktop_platform,
      connected,
      capabilities,
    });
    try {
      await copyText(summary);
      setCopied(true);
      if (copy_timer_ref.current !== null) {
        window.clearTimeout(copy_timer_ref.current);
      }
      copy_timer_ref.current = window.setTimeout(() => {
        setCopied(false);
        copy_timer_ref.current = null;
      }, 2000);
      root.settings.showNotice("环境信息已复制。");
    } catch (error: unknown) {
      root.settings.showError(error instanceof Error ? error.message : "复制环境信息失败。");
    }
  }

  function handleOpenFeedback() {
    root.settings.close();
    root.feedback_center.show();
  }

  if (view === "changelog") {
    return (
      <SettingsPageContainer
        back_label="返回关于"
        on_back={() => setView("overview")}
        title="更新日志"
      >
        <div className={styles.release_stack}>
          {RELEASE_CHANGELOG.map((release) => (
            <article className={styles.release_card} key={release.version}>
              <header className={styles.release_card_header}>
                <div className={styles.release_title}>
                  <h4>v{release.version}</h4>
                  {release.version === __APP_VERSION__ ? (
                    <span className={styles.current_tag}>当前版本</span>
                  ) : null}
                </div>
                <span className={styles.release_date}>{release.date}</span>
              </header>
              <p className={styles.release_summary}>{release.summary}</p>
              <ul className={styles.release_points}>
                {release.items.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </article>
          ))}
        </div>
      </SettingsPageContainer>
    );
  }

  if (view === "features") {
    return (
      <SettingsPageContainer
        back_label="返回关于"
        on_back={() => setView("overview")}
        title="App 功能"
      >
        <ul className={styles.feature_list}>
          {APP_CAPABILITIES.map((feature) => (
            <li className={styles.feature_item} key={feature.id}>
              <span aria-hidden="true" className={styles.feature_icon}>
                <Icon name={feature.icon} size={16} />
              </span>
              <div className={styles.feature_copy}>
                <strong>{feature.title}</strong>
                <p>{feature.description}</p>
              </div>
            </li>
          ))}
        </ul>
      </SettingsPageContainer>
    );
  }

  if (view === "licenses") {
    return (
      <SettingsPageContainer
        back_label="返回关于"
        on_back={() => setView("overview")}
        title="开源许可与致谢"
      >
        <ul className={styles.license_list}>
          {LICENSE_ATTRIBUTIONS.map((item) => (
            <li className={styles.license_item} key={item.name}>
              <div className={styles.license_copy}>
                <strong>{item.name}</strong>
                <span>{item.usage}</span>
              </div>
              <span className={styles.license_badge}>{item.license}</span>
            </li>
          ))}
        </ul>
      </SettingsPageContainer>
    );
  }

  return (
    <SettingsPageContainer title="关于">
      <div className={styles.root}>
        <section aria-label="版本信息" className={styles.hero}>
          <img alt="" aria-hidden="true" className={styles.logo} draggable={false} src={appLogo} />
          <div className={styles.brand_copy}>
            <h4 className={styles.product_name}>EZ Assistant</h4>
            <p className={styles.copyright}>Copyright © 2026 EZ Assistant. 保留所有权利。</p>
          </div>
          <div className={styles.version_bar}>
            <span className={styles.version_pill}>
              {is_desktop ? "客户端" : "Web 端"} v{__APP_VERSION__}
            </span>
            {capabilities?.runtime_version ? (
              <span className={styles.version_pill} data-connected={connected}>
                <i aria-hidden="true" className={styles.status_dot} />
                Runtime v{capabilities.runtime_version}
              </span>
            ) : null}
            <Button onClick={() => void handleCopyEnvironment()} size="small" variant="text">
              <Icon name={copied ? "check" : "copy"} size={14} />
              {copied ? "已复制" : "复制环境"}
            </Button>
            {has_update && app_connection ? (
              <Button
                onClick={() => app_connection.updates.show()}
                size="small"
                variant="primary"
              >
                发现新版本
              </Button>
            ) : null}
          </div>
        </section>

        <section aria-label="当前版本特性" className={styles.section}>
          <header className={styles.section_header}>
            <h4>当前版本特性</h4>
            <span>
              v{CURRENT_RELEASE_META.version} · {CURRENT_RELEASE_META.date}
            </span>
          </header>
          <ul className={styles.feature_list}>
            {CURRENT_VERSION_FEATURES.map((feature) => (
              <li className={styles.feature_item} key={feature.id}>
                <span aria-hidden="true" className={styles.feature_icon}>
                  <Icon name={feature.icon} size={16} />
                </span>
                <div className={styles.feature_copy}>
                  <strong>{feature.title}</strong>
                  <p>{feature.description}</p>
                </div>
              </li>
            ))}
          </ul>
        </section>

        <div className={styles.links_container}>
          <div className={styles.link_group}>
            <button
              className={styles.link_row}
              onClick={() => setView("changelog")}
              type="button"
            >
              <div className={styles.link_copy}>
                <strong>更新日志</strong>
                <span>查看每个版本的新功能与改进</span>
              </div>
              <Icon name="chevron-right" size={16} />
            </button>
            <button
              className={styles.link_row}
              onClick={() => setView("features")}
              type="button"
            >
              <div className={styles.link_copy}>
                <strong>App 功能</strong>
                <span>App 核心能力概览与工作方式</span>
              </div>
              <Icon name="sparkles" size={16} />
            </button>
            {has_feedback_center ? (
              <button
                className={styles.link_row}
                disabled={!connected}
                onClick={handleOpenFeedback}
                type="button"
              >
                <div className={styles.link_copy}>
                  <strong>发送反馈</strong>
                  <span>告诉我们哪些地方还可以改进</span>
                </div>
                <Icon name="message" size={16} />
              </button>
            ) : null}
          </div>

          <div className={styles.link_group_secondary}>
            <button
              className={styles.link_row}
              onClick={() => props.onNavigate("runtime_diagnostics")}
              type="button"
            >
              <div className={styles.link_copy}>
                <strong>状态与诊断</strong>
                <span>查看 Runtime 连接、兼容版本与系统诊断</span>
              </div>
              <Icon name="chevron-right" size={16} />
            </button>
            <button
              className={styles.link_row}
              onClick={() => setView("licenses")}
              type="button"
            >
              <div className={styles.link_copy}>
                <strong>开源许可与致谢</strong>
                <span>查看随包第三方依赖与许可说明</span>
              </div>
              <Icon name="chevron-right" size={16} />
            </button>
          </div>
        </div>

        <SettingsMessages />
      </div>
    </SettingsPageContainer>
  );
});
