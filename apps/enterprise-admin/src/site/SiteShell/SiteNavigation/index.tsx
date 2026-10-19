import { useEffect, useState } from 'react';
import { Link, NavLink } from 'react-router';
import { ArrowRightOutlined } from '@ant-design/icons';
import { DownloadDialog } from '../../DownloadDialog';
import { previewMode } from '../../request';
import logo from '../../assets/app-icon.png';
import type { SiteContent } from '../../../request/openapi';
import styles from './index.module.scss';

export function SiteNavigation({ home }: { readonly home: SiteContent['home'] }) {
  const [downloadOpen, setDownloadOpen] = useState(false);
  const [stuck, setStuck] = useState(() => window.scrollY > 18);

  useEffect(() => {
    // 只记录是否越过顶部留白，导航不占用文档流，切换时正文不会跳动。
    const updatePosition = () => setStuck(window.scrollY > 18);
    updatePosition();
    window.addEventListener('scroll', updatePosition, { passive: true });
    return () => window.removeEventListener('scroll', updatePosition);
  }, []);

  return (
    <>
      <div className={styles.bar} data-stuck={stuck}>
        <header className={styles.header}>
          <Link className={styles.brand} to="/">
            <img src={logo} alt="" />
            <span>EZ Assistant</span>
          </Link>
          <nav aria-label="主导航" className={styles.nav}>
            {home.some((section) => section.kind === 'capabilities') && <Link to="/#capabilities">插件能力</Link>}
            {home.some((section) => section.kind === 'modes') && <Link to="/#usage-modes">使用模式</Link>}
            <button onClick={() => setDownloadOpen(true)}>下载</button>
            <NavLink to="/changelog">更新日志</NavLink>
            <NavLink to="/manual">操作手册</NavLink>
          </nav>
          {previewMode ? (
            <span>本地内容预览</span>
          ) : (
            <a className={styles.admin} href="/admin/">
              管理后台 <ArrowRightOutlined />
            </a>
          )}
        </header>
      </div>
      <DownloadDialog open={downloadOpen} onClose={() => setDownloadOpen(false)} />
    </>
  );
}
