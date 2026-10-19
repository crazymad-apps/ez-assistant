import { useState } from 'react';
import { Link, useOutletContext } from 'react-router';
import { Button } from 'antd';
import {
  AppleOutlined,
  WindowsOutlined,
  DownloadOutlined,
  BookOutlined,
  FileTextOutlined,
  ArrowRightOutlined,
} from '@ant-design/icons';
import { contentImages } from '../request';
import type { SiteContent } from '../../request/openapi';
import { useDocumentPosition } from '../useDocumentPosition';
import type { useSiteQuery } from '../useSiteQuery';
import { SiteDocument } from '../SiteDocument';
import { PageState } from '../PageState';
import { documentSections } from '../document';
import { ResultPreview } from '../ResultPreview';
import { WorkspacePreview } from '../WorkspacePreview';
import { DownloadDialog } from '../DownloadDialog';
import { useDownloadPlatform } from '../DownloadOptions/useDownloadPlatform';
import { DotsBackground } from './DotsBackground';
import { UsageModes } from '../UsageModes';
import logo from '../assets/app-icon.png';
import styles from './index.module.scss';

export function Home() {
  const query = useOutletContext<ReturnType<typeof useSiteQuery<SiteContent | null>>>();
  const [downloadOpen, setDownloadOpen] = useState(false);
  const device = useDownloadPlatform();
  useDocumentPosition(query.value);
  if (query.loading || query.error || !query.value?.home.length)
    return <PageState loading={query.loading} error={query.error} title="首页内容尚未发布" onRetry={query.reload} />;
  const images = contentImages(query.value);
  const scenarios = query.value.home.find((section) => section.kind === 'workspace')?.scenarios ?? [];
  return (
    <div className={styles.page}>
      <div className={styles.decoration} aria-hidden="true">
        <DotsBackground />
      </div>
      <main id="main" tabIndex={-1}>
        {query.value.home.map((section) => {
          const body = <SiteDocument body={section.body} images={images} preview={false} />;
          switch (section.kind) {
            case 'hero':
              return (
                <section key={section.kind} className={styles.hero}>
                  <div className={styles.intro}>
                    <span /> 个人灵活使用 · 企业统一分发
                  </div>
                  <h1>{section.title}</h1>
                  <p>{section.description}</p>
                  {body}
                  <div className={styles.heroActions}>
                    <Button
                      type="primary"
                      size="large"
                      icon={<DownloadOutlined />}
                      onClick={() => setDownloadOpen(true)}
                    >
                      {device.label}
                    </Button>
                    <Button size="large" icon={<BookOutlined />} href="/manual">
                      开始使用
                    </Button>
                  </div>
                  <div className={styles.platforms}>
                    <AppleOutlined /> macOS <span />
                    <WindowsOutlined /> Windows <span /> 本地优先
                  </div>
                </section>
              );
            case 'workspace':
              return (
                <section key={section.kind} aria-label={section.title}>
                  <WorkspacePreview scenarios={scenarios} />
                  {section.body.length > 0 && <div className={styles.capabilities}>{body}</div>}
                </section>
              );
            case 'capabilities':
              return (
                <section key={section.kind} id="capabilities" className={styles.capabilities}>
                  <div className={styles.sectionHeading}>
                    <h2>{section.title}</h2>
                    <p>{section.description}</p>
                  </div>
                  <div className={styles.features}>
                    {documentSections(section.body).map((card, index) => (
                      <article key={card.id || index}>
                        {scenarios
                          .filter((scene) => scene.id === card.id)
                          .map((scene) => (
                            <div className={styles.featureVisual} key={scene.id}>
                              <ResultPreview scenario={scene} />
                            </div>
                          ))}
                        <div className={styles.featureCopy}>
                          <h3>{card.title}</h3>
                          <SiteDocument body={card.body} images={images} preview={false} />
                        </div>
                      </article>
                    ))}
                  </div>
                  <p className={styles.pluginNotice}>专业能力由插件扩展，按需安装，随工作一起成长。</p>
                </section>
              );
            case 'modes':
              return <UsageModes key={section.kind} section={section} images={images} />;
            case 'resources':
              return (
                <section key={section.kind} className={styles.resources} aria-label="产品资料">
                  <div className={styles.resourceHeading}>
                    <span>{section.title}</span>
                    <p>{section.description}</p>
                    {body}
                  </div>
                  <Link to="/changelog">
                    <FileTextOutlined />
                    <div>
                      <h3>看看最近的更新</h3>
                      <p>发现助手的新本领。</p>
                    </div>
                    <ArrowRightOutlined />
                  </Link>
                  <Link to="/manual">
                    <BookOutlined />
                    <div>
                      <h3>找到你的使用方式</h3>
                      <p>从第一个任务，开启你的工作方式。</p>
                    </div>
                    <ArrowRightOutlined />
                  </Link>
                </section>
              );
            case 'closing':
              return (
                <section key={section.kind} className={styles.closing}>
                  <div className={styles.closingCopy}>
                    <div className={styles.closingBrand}>
                      <img src={logo} alt="" />
                      <span>EZ Assistant</span>
                    </div>
                    <h2>{section.title}</h2>
                    <p>{section.description}</p>
                    {body}
                    <Button
                      type="primary"
                      size="large"
                      icon={<DownloadOutlined />}
                      onClick={() => setDownloadOpen(true)}
                    >
                      {device.label}
                    </Button>
                    <div className={styles.closingPlatforms}>
                      <AppleOutlined /> macOS <WindowsOutlined /> Windows
                    </div>
                  </div>
                  <div className={styles.deliverables} aria-label="交付成果示意">
                    {scenarios
                      .filter((scene) => scene.kind !== 'notes')
                      .slice(0, 3)
                      .map((scene) => (
                        <div key={scene.id} className={`${styles.document} ${styles[scene.kind]}`}>
                          <div className={styles.documentTitle}>
                            <FileTextOutlined />
                            <span>{scene.file}</span>
                          </div>
                          <ResultPreview scenario={scene} />
                        </div>
                      ))}
                  </div>
                </section>
              );
          }
        })}
      </main>
      <DownloadDialog open={downloadOpen} onClose={() => setDownloadOpen(false)} />
    </div>
  );
}
