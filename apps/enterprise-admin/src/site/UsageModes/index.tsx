import { CloudServerOutlined, LaptopOutlined, ArrowDownOutlined } from '@ant-design/icons';
import type { SiteHomeSection } from '../../request/openapi';
import { SiteDocument } from '../SiteDocument';
import styles from './index.module.scss';

export function UsageModes({
  section,
  images,
}: {
  readonly section: SiteHomeSection;
  readonly images: Record<string, string>;
}) {
  return (
    <section id="usage-modes" className={styles.section} aria-labelledby="usage-modes-title">
      <div className={styles.overview}>
        <header className={styles.heading}>
          <span>从个人使用，到企业协作</span>
          <h2 id="usage-modes-title">{section.title}</h2>
          <p>{section.description}</p>
        </header>
        <div className={styles.distribution}>
          <div className={styles.center}>
            <CloudServerOutlined aria-hidden="true" />
            <div>
              <h3>企业中心</h3>
              <p>把好用的能力，交给整个团队。</p>
            </div>
            <ul aria-label="统一分发">
              <li>模型服务</li>
              <li>插件市场</li>
              <li>智能体配置</li>
              <li>客户端安装包</li>
            </ul>
          </div>
          <div className={styles.connection}>
            <ArrowDownOutlined aria-hidden="true" />
            <span>统一准备 · 按需使用 · 团队共享</span>
          </div>
          <div className={styles.members}>
            {['售前方案', '项目交付', '售后支持'].map((label) => (
              <div key={label}>
                <LaptopOutlined aria-hidden="true" />
                <span>{label}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className={styles.comparison}>
        <SiteDocument body={section.body} images={images} preview={false} />
      </div>
    </section>
  );
}
