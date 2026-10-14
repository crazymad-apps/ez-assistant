import { CloudServerOutlined, LaptopOutlined, TeamOutlined, ArrowDownOutlined } from '@ant-design/icons';
import { modeComparison } from './content';
import styles from './index.module.scss';

export function UsageModes() {
  return (
    <section id="usage-modes" className={styles.section} aria-labelledby="usage-modes-title">
      <div className={styles.overview}>
        <header className={styles.heading}>
          <span>从个人使用，到企业协作</span>
          <h2 id="usage-modes-title">
            <span>个人灵活配置，</span>
            <span>企业统一分发。</span>
          </h2>
          <p>把成熟的工具与工作方法交给企业中心维护，让每位成员从准备好的能力开始。</p>
        </header>
        <div className={styles.distribution}>
          <div className={styles.center}>
            <CloudServerOutlined aria-hidden="true" />
            <div>
              <h3>企业中心</h3>
              <p>管理员统一准备，成员按需使用。</p>
            </div>
            <ul aria-label="企业中心提供的能力">
              <li>模型服务</li>
              <li>插件市场</li>
              <li>智能体配置</li>
              <li>客户端安装包</li>
            </ul>
          </div>
          <div className={styles.connection}>
            <ArrowDownOutlined aria-hidden="true" />
            <span>企业账号登录 · 获取可用能力 · 按需安装与更新</span>
          </div>
          <div className={styles.members} aria-label="团队成员使用场景">
            <div>
              <LaptopOutlined aria-hidden="true" />
              <span>售前方案</span>
            </div>
            <div>
              <LaptopOutlined aria-hidden="true" />
              <span>项目交付</span>
            </div>
            <div>
              <LaptopOutlined aria-hidden="true" />
              <span>售后支持</span>
            </div>
          </div>
        </div>
      </div>
      <div className={styles.comparison}>
        <table>
          <caption>选择适合你的使用模式</caption>
          <thead>
            <tr>
              <th scope="col">使用方式</th>
              <th scope="col">
                <LaptopOutlined aria-hidden="true" /> 个人模式<small>自己配置，灵活探索</small>
              </th>
              <th scope="col">
                <TeamOutlined aria-hidden="true" /> 企业模式<small>中心管理，团队复用</small>
              </th>
            </tr>
          </thead>
          <tbody>
            {modeComparison.map((row) => (
              <tr key={row.label}>
                <th scope="row">{row.label}</th>
                <td>{row.personal}</td>
                <td>{row.enterprise}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <p className={styles.note}>
        企业中心负责账号、模型服务与能力分发；任务在连接的工作环境中执行，企业用户数据按账号隔离。
      </p>
    </section>
  );
}
