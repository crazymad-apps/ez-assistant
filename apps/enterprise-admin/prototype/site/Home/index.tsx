import { useState } from 'react';
import { Link } from 'react-router';
import { Button } from 'antd';
import {
  ArrowRightOutlined,
  DownloadOutlined,
  FileTextOutlined,
  AppleOutlined,
  WindowsOutlined,
  BookOutlined,
} from '@ant-design/icons';
import { pluginCapabilities, scenarios } from '../content';
import { ResultPreview } from '../ResultPreview';
import { WorkspacePreview } from '../WorkspacePreview';
import { DownloadDialog } from '../DownloadDialog';
import logo from '../assets/app-icon.svg';
import styles from './index.module.scss';
import { DotsBackground } from './DotsBackground';
import { UsageModes } from '../UsageModes';

export function Home() {
  const [downloadOpen, setDownloadOpen] = useState(false);

  return (
    <div className={styles.page}>
      <div className={styles.decoration} aria-hidden="true">
        <DotsBackground />
      </div>
      <main id="main">
        <section className={styles.hero}>
          <div className={styles.intro}>
            <span /> 个人灵活使用 · 企业统一分发
          </div>
          <h1>
            <span>从项目资料，</span>
            <span>到方案交付。</span>
          </h1>
          <p>
            画库房布局、看三维方案、做 Excel 报价。
            <br />
            企业中心统一提供模型、插件与智能体，让团队用上同一套专业能力。
          </p>
          <div className={styles.heroActions}>
            <Button type="primary" size="large" icon={<DownloadOutlined />} onClick={() => setDownloadOpen(true)}>
              下载 EZ Assistant
            </Button>
            <Button size="large" icon={<BookOutlined />} href="#/manual">
              阅读使用指南
            </Button>
          </div>
          <div className={styles.platforms}>
            <AppleOutlined /> macOS <span />
            <WindowsOutlined /> Windows <span /> 本地优先
          </div>
        </section>
        <WorkspacePreview />
        <section id="capabilities" className={styles.capabilities}>
          <div className={styles.sectionHeading}>
            <h2>
              从绘图到报价，
              <br />
              围绕真实项目展开。
            </h2>
            <p>
              按任务选用插件，也可以组合使用。
              <br />
              报价可以直接从需求与方案资料开始。
            </p>
          </div>
          <div className={styles.features}>
            {pluginCapabilities.map((plugin) => (
              <article key={plugin.id}>
                <div className={styles.featureVisual}>
                  {scenarios
                    .filter((scenario) => scenario.id === plugin.id)
                    .map((scenario) => (
                      <ResultPreview key={scenario.id} scenario={scenario} />
                    ))}
                </div>
                <div className={styles.featureCopy}>
                  <div className={styles.pluginName}>{plugin.name}</div>
                  <h3>{plugin.title}</h3>
                  <p>{plugin.description}</p>
                  <strong className={styles.output}>{plugin.output}</strong>
                  <span className={styles.condition}>{plugin.note}</span>
                </div>
              </article>
            ))}
          </div>
          <p className={styles.pluginNotice}>
            以上展示插件能力示例，使用前需安装并启用对应插件，支持平台以各插件说明为准。
          </p>
        </section>
        <UsageModes />
        <section className={styles.resources} aria-label="产品资料">
          <div className={styles.resourceHeading}>
            <span>持续完善，也容易上手。</span>
            <p>了解最近的变化，或者从第一步开始。</p>
          </div>
          <Link to="/changelog">
            <FileTextOutlined />
            <div>
              <h3>看看最近的更新</h3>
              <p>新增能力、体验改进与问题修复。</p>
            </div>
            <ArrowRightOutlined />
          </Link>
          <Link to="/manual">
            <BookOutlined />
            <div>
              <h3>找到你的使用方式</h3>
              <p>从安装配置，到完成第一个任务。</p>
            </div>
            <ArrowRightOutlined />
          </Link>
        </section>
        <section className={styles.closing} aria-label="开始使用 EZ Assistant">
          <div className={styles.closingCopy}>
            <div className={styles.closingBrand}>
              <img src={logo} alt="" />
              <span>EZ Assistant</span>
            </div>
            <h2>
              <span>下一件事，</span>
              <span>一起完成。</span>
            </h2>
            <p>从一份图纸、一份需求，或一个具体问题开始。</p>
            <Button type="primary" size="large" icon={<DownloadOutlined />} onClick={() => setDownloadOpen(true)}>
              下载桌面客户端
            </Button>
            <div className={styles.closingPlatforms}>
              <AppleOutlined /> macOS <WindowsOutlined /> Windows
            </div>
          </div>
          <div className={styles.deliverables} aria-label="图纸、三维方案与报价单交付示意">
            {scenarios.slice(0, 3).map((scenario) => (
              <div key={scenario.id} className={`${styles.document} ${styles[scenario.kind]}`}>
                <div className={styles.documentTitle}>
                  <FileTextOutlined />
                  <span>{scenario.file}</span>
                </div>
                <ResultPreview scenario={scenario} />
              </div>
            ))}
          </div>
        </section>
      </main>
      <DownloadDialog open={downloadOpen} onClose={() => setDownloadOpen(false)} />
    </div>
  );
}
