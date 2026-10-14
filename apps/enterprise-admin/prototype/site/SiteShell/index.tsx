import { useEffect } from 'react';
import { Link, Outlet, useLocation } from 'react-router';
import { SiteNavigation } from './SiteNavigation';
import styles from './index.module.scss';

/** 仅装配原型导航；路由切换恢复阅读位置，不读取任何后台状态。 */
export function SiteShell() {
  const location = useLocation();
  useEffect(() => {
    const target = location.hash ? document.getElementById(location.hash.slice(1)) : null;
    if (target) target.scrollIntoView();
    else window.scrollTo({ top: 0, behavior: 'instant' });
    let title = '从项目资料，到方案交付';
    if (location.pathname.startsWith('/manual')) title = '操作手册';
    if (location.pathname.startsWith('/changelog')) title = '更新日志';
    document.title = `${title} · EZ Assistant`;
  }, [location]);
  return (
    <>
      <Link className={styles.skip} to={{ pathname: location.pathname, hash: '#main' }}>
        跳转到主要内容
      </Link>
      <SiteNavigation />
      <Outlet />
      <footer className={styles.footer}>
        <Link to="/">EZ Assistant</Link>
        <span>本地优先，让工作自然发生。</span>
        <span className={styles.preview}>页面设计预览 · 演示内容</span>
      </footer>
    </>
  );
}
