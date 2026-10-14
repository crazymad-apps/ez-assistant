import { Link } from 'react-router';
import { ArrowRightOutlined, FileTextOutlined } from '@ant-design/icons';
import { releases } from './content';
import styles from './index.module.scss';

export function Changelog() {
  return (
    <main id="main" className={styles.page}>
      <header className={styles.heading}>
        <div>
          <div className={styles.label}>
            <FileTextOutlined /> 更新日志
          </div>
          <h1>
            每一次更新，
            <br />
            让工作更顺手。
          </h1>
          <p>看看新增的能力、体验改进，以及那些被修好的小问题。</p>
        </div>
        <Link to="/manual">
          了解如何使用 <ArrowRightOutlined />
        </Link>
      </header>
      <div className={styles.demo_note}>设计预览 · 以下版本号、日期及更新内容均为示例。</div>
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
                <span>{release.version}</span>
                <time>{release.date}</time>
                {index === 0 && <small>本期示例</small>}
              </div>
              <h2>{release.title}</h2>
              <p className={styles.summary}>{release.summary}</p>
              <div className={styles.changes}>
                {release.changes.map((change) => (
                  <div key={change.title} className={styles.change}>
                    <span data-kind={change.kind}>{change.kind}</span>
                    <div>
                      <h3>{change.title}</h3>
                      <p>{change.body}</p>
                    </div>
                  </div>
                ))}
              </div>
            </article>
          ))}
          <div className={styles.end}>每一个小改进，都为下一次使用。</div>
        </div>
      </div>
    </main>
  );
}
