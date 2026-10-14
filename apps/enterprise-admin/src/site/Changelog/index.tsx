import { Link } from 'react-router';
import { DownloadOutlined } from '@ant-design/icons';
import { readChangelog, previewMode } from '../request';
import { useSiteQuery } from '../useSiteQuery';
import { PageState } from '../PageState';
import { SiteDocument } from '../SiteDocument';
import { useDocumentPosition } from '../useDocumentPosition';
import styles from './index.module.scss';

export function Changelog() {
  const query = useSiteQuery(readChangelog);
  useDocumentPosition(query.value);
  if (query.loading || query.error || !query.value?.length)
    return (
      <PageState
        loading={query.loading}
        error={query.error}
        title={previewMode ? '本地预览不读取中心更新日志' : '暂无已发布的更新日志'}
        onRetry={query.reload}
      />
    );
  const releases = query.value.map((release) => ({
    ...release,
    id: `v${release.version}`,
    date: new Date(release.published_at).toLocaleDateString('zh-CN'),
  }));
  return (
    <main id="main" tabIndex={-1} className={styles.page}>
      <h1 className={styles.page_title}>更新日志</h1>
      <div className={styles.layout}>
        <aside className={styles.index}>
          <nav aria-label="版本目录">
            <span>版本记录</span>
            {releases.map((release) => (
              <Link key={release.id} to={`/changelog#${release.id}`}>
                {release.version}
                <small>{release.date}</small>
              </Link>
            ))}
          </nav>
        </aside>
        <div className={styles.timeline}>
          {releases.map((release, index) => (
            <article key={release.id} id={release.id} className={styles.release}>
              <div className={styles.release_meta}>
                <h2 className={styles.version}>{release.version}</h2>
                <time>{release.date}</time>
                {index === 0 && <small>最新记录</small>}
              </div>
              <SiteDocument body={release.body} preview={false} headingPrefix={`${release.id}-`} />
              <footer className={styles.release_actions}>
                <Link className={styles.package_link} to={`/download?version=${release.version}`}>
                  <DownloadOutlined /> 查看此版本安装包
                </Link>
              </footer>
            </article>
          ))}
          <div className={styles.end}>每一个小改进，都为下一次使用。</div>
        </div>
      </div>
    </main>
  );
}
