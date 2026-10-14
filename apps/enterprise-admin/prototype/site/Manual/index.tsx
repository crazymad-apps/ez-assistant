import { useState } from 'react';
import { Link, NavLink, useParams } from 'react-router';
import { ArrowLeftOutlined, ArrowRightOutlined, BookOutlined, DownOutlined } from '@ant-design/icons';
import { articles } from './content';
import styles from './index.module.scss';

export function Manual() {
  const { articleId } = useParams();
  const [directoryOpen, setDirectoryOpen] = useState(false);
  const index = articles.findIndex((article) => article.id === articleId);
  const article = articles[index];
  if (!article)
    return (
      <main id="main" className={styles.missing}>
        <h1>没有找到这篇文章</h1>
        <Link to="/manual">返回操作手册</Link>
      </main>
    );
  const previous = articles[index - 1];
  const next = articles[index + 1];
  return (
    <main id="main" className={styles.page}>
      <div className={styles.page_label}>
        <BookOutlined /> 操作手册 <span>设计预览 · 示例内容，非正式操作指引</span>
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
          <p className={styles.intro}>{article.intro}</p>
          <div className={styles.metadata}>
            适用版本：示例版 <span>约 3 分钟阅读</span>
          </div>
          {article.sections.map((section) => (
            <section key={section.id} id={section.id} className={styles.section}>
              <h2>
                <Link to={`/manual/${article.id}#${section.id}`}>{section.title}</Link>
              </h2>
              <p>{section.body}</p>
              {section.steps && (
                <ol>
                  {section.steps.map((step) => (
                    <li key={step}>{step}</li>
                  ))}
                </ol>
              )}
              {section.note && <blockquote>{section.note}</blockquote>}
            </section>
          ))}
          <div className={styles.article_end}>准备好后，可以带上一份资料，试着完成一个具体任务。</div>
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
            {article.sections.map((section) => (
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
