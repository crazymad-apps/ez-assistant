import { useEffect } from 'react';
import { Link, Outlet, useLocation } from 'react-router';
import { SiteNavigation } from './SiteNavigation';
import styles from './index.module.scss';
import { previewMode, readContent } from '../request';
import { useSiteQuery } from '../useSiteQuery';

/** 仅装配公开导航；路由切换恢复阅读位置，不读取任何后台状态。 */
export function SiteShell() {
  const location = useLocation();
  // 手册切换共享同一快照；重新进入手册或显式刷新时才读取新的静态内容。
  const manualRoute = location.pathname === '/manual' || location.pathname.startsWith('/manual/');
  const content = useSiteQuery(readContent, manualRoute ? '/manual' : location.pathname);
  useEffect(() => {
    // 手册由正文定位处理标题和锚点，避免先跳页顶、再跳正文引起整页闪动。
    if (!manualRoute) {
      const target = location.hash ? document.getElementById(location.hash.slice(1)) : null;
      if (target) target.scrollIntoView();
      else {
        window.scrollTo({ top: 0, behavior: 'instant' });
        document.getElementById('main')?.focus({ preventScroll: true });
      }
    }
    document.title = 'ez-assistant助手';
  }, [location, manualRoute]);
  return (
    <>
      <Link className={styles.skip} to={{ pathname: location.pathname, hash: '#main' }}>
        跳转到主要内容
      </Link>
      <SiteNavigation home={content.value?.home ?? []} />
      <Outlet context={content} />
      <footer className={styles.footer}>
        <Link to="/">EZ Assistant</Link>
        <span>本地优先，让工作自然发生。</span>
        {previewMode && <span className={styles.preview}>本地内容预览 · 下载已禁用</span>}
      </footer>
    </>
  );
}
