import { useState } from 'react';
import { Alert } from 'antd';
import { Link, NavLink, Navigate, useOutletContext, useParams } from 'react-router';
import { ArrowLeftOutlined, ArrowRightOutlined, BookOutlined, DownOutlined } from '@ant-design/icons';
import { contentImages } from '../request';
import type { SiteContent } from '../../request/openapi';
import type { useSiteQuery } from '../useSiteQuery';
import { PageState } from '../PageState';
import { SiteDocument } from '../SiteDocument';
import { nodeText } from '../document';
import { useDocumentPosition } from '../useDocumentPosition';
import styles from './index.module.scss';

export function Manual() {
  const { articleId } = useParams();
  const [directoryOpen, setDirectoryOpen] = useState(false);
  // 手册各篇来自同一内容快照，切换文章只更新投影，不重新加载并卸载目录。
  const query = useOutletContext<ReturnType<typeof useSiteQuery<SiteContent | null>>>();
  const missingAnchor = useDocumentPosition(query.loading ? undefined : query.value, {
    pageTop: true,
    behavior: 'smooth',
  });
  if (query.loading || query.error || !query.value?.manual.length)
    return <PageState loading={query.loading} error={query.error} title="操作手册尚未发布" onRetry={query.reload} />;
  const articles = query.value.manual.flatMap((chapter) =>
    chapter.articles.map((article) => ({ ...article, group: chapter.title })),
  );
  const index = articleId ? articles.findIndex((article) => article.id === articleId) : 0;
  const article = articles[index];
  if (!article)
    return (
      <main id="main" tabIndex={-1} className={styles.missing}>
        <h1>没有找到这篇文章</h1>
        <Link to="/manual">返回操作手册</Link>
      </main>
    );
  if (!articleId) return <Navigate to={`/manual/${article.id}`} replace />;
  const sections = article.body
    .filter((node) => node.kind === 'heading' && node.id)
    .map((node) => ({ id: node.id!, title: nodeText(node) }));
  const previous = articles[index - 1];
  const next = articles[index + 1];
  return (
    <main id="main" tabIndex={-1} className={styles.page}>
      <div className={styles.page_label}>
        <BookOutlined /> 操作手册{' '}
        <span>
          <button onClick={query.reload}>刷新内容</button>
        </span>
      </div>
      <div className={styles.layout}>
        <aside className={styles.sidebar}>
          <button
            className={styles.directory_toggle}
            aria-expanded={directoryOpen}
            aria-controls="manual-directory"
            onClick={() => setDirectoryOpen(!directoryOpen)}
          >
            章节目录 <DownOutlined />
          </button>
          <nav id="manual-directory" className={styles.chapter_nav} data-open={directoryOpen} aria-label="手册章节">
            {[...new Set(articles.map((item) => item.group))].map((group) => (
              <div key={group}>
                <h2>{group}</h2>
                {articles
                  .filter((item) => item.group === group)
                  .map((item) => (
                    <NavLink key={item.id} to={`/manual/${item.id}`} onClick={() => setDirectoryOpen(false)}>
                      {item.title}
                    </NavLink>
                  ))}
              </div>
            ))}
            <Link className={styles.home_link} to="/">
              <ArrowLeftOutlined /> 返回首页
            </Link>
          </nav>
        </aside>
        <article className={styles.article}>
          <div className={styles.breadcrumb}>
            操作手册 <span>/</span> {article.group}
          </div>
          <h1>{article.title}</h1>
          <div className={styles.metadata}>
            适用版本：{article.applies_to} <span>更新于 {article.date}</span>
          </div>
          {missingAnchor && <Alert type="info" title="此段落不存在或已更新，已显示文章正文。" />}
          <div className={styles.section}>
            <SiteDocument body={article.body} images={contentImages(query.value)} preview={false} />
          </div>
          <nav className={styles.article_pager} aria-label="相邻文章">
            {previous ? (
              <Link to={`/manual/${previous.id}`}>
                <span>
                  <ArrowLeftOutlined /> 上一篇
                </span>
                <strong>{previous.title}</strong>
              </Link>
            ) : (
              <span />
            )}
            {next ? (
              <Link to={`/manual/${next.id}`}>
                <span>
                  下一篇 <ArrowRightOutlined />
                </span>
                <strong>{next.title}</strong>
              </Link>
            ) : (
              <Link to="/changelog">
                <span>
                  继续了解 <ArrowRightOutlined />
                </span>
                <strong>查看更新日志</strong>
              </Link>
            )}
          </nav>
        </article>
        <aside className={styles.toc}>
          <nav aria-label="本页内容">
            <span>本页内容</span>
            {sections.map((section) => (
              <Link key={section.id} to={`/manual/${article.id}#${section.id}`}>
                {section.title}
              </Link>
            ))}
          </nav>
        </aside>
      </div>
    </main>
  );
}
