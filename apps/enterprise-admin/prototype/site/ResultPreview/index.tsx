import type { scenarios } from '../content';
import styles from './index.module.scss';

type ResultPreviewProps = { readonly scenario: (typeof scenarios)[number] };

/** 用合成矢量和表格说明交付类型，不复用客户图纸或冒充实际插件执行结果。 */
export function ResultPreview({ scenario }: ResultPreviewProps) {
  if (scenario.kind === 'drawing') {
    return (
      <figure className={styles.figure}>
        <svg viewBox="0 0 260 170" role="img" aria-label="密集架平面布局示意">
          <path d="M20 140V20H240V140H157M113 140H20" fill="none" stroke="#536c87" strokeWidth="3" />
          {[38, 72, 106, 168, 202].map((x) => (
            <g key={x}>
              <rect x={x} y="40" width="23" height="76" fill="#edf3fd" stroke="#2e81d8" />
              {[59, 78, 97].map((y) => (
                <path key={y} d={`M${x} ${y}h23`} stroke="#89acd2" />
              ))}
            </g>
          ))}
          <path d="M113 140v-40M113 100a40 40 0 0 1 40 40" fill="none" stroke="#94a9bd" strokeDasharray="3 3" />
          <text x="139" y="76" textAnchor="middle" fontSize="10" fill="#687389" writingMode="tb">
            通道
          </text>
          <text x="130" y="160" textAnchor="middle" fontSize="10" fill="#687389">
            入口
          </text>
        </svg>
        <figcaption>平面布局示意 · 非施工图</figcaption>
      </figure>
    );
  }

  if (scenario.kind === 'scene') {
    return (
      <figure className={styles.figure}>
        <svg viewBox="0 0 260 180" role="img" aria-label="密集架三维空间示意">
          <path d="M16 112L121 54L244 116L140 175Z" fill="#eaf0f7" />
          <path d="M16 112V57L121 0V54Z" fill="#f0f4f8" />
          <path d="M121 0L244 63V116L121 54Z" fill="#f7f9fc" />
          {[0, 1, 2, 3].map((column) => (
            <g key={column} transform={`translate(${46 + column * 31},${70 + column * 15})`}>
              <path d="M0 0L50 -27L64 -20L14 7Z" fill="#b5c8d9" stroke="#8da7bd" />
              <path d="M0 0L14 7V66L0 59Z" fill="#354858" />
              <path d="M14 7L64 -20V39L14 66Z" fill="#d4dfe8" stroke="#8da7bd" />
              {[17, 28, 39, 50].map((y) => (
                <path key={y} d={`M14 ${y}l50 -27`} stroke="#9aafc1" />
              ))}
              <rect x="4" y="19" width="6" height="5" fill="#90c7f0" />
            </g>
          ))}
        </svg>
        <figcaption>空间效果示意 · 实际插件支持旋转查看</figcaption>
      </figure>
    );
  }

  if (scenario.kind === 'quote') {
    return (
      <figure className={styles.figure}>
        <table className={styles.quote}>
          <caption>项目报价单</caption>
          <thead>
            <tr>
              <th>产品</th>
              <th>规格</th>
              <th>金额</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>密集架</td>
              <td>按方案</td>
              <td>待核定</td>
            </tr>
            <tr>
              <td>环控设备</td>
              <td>按需求</td>
              <td>待核定</td>
            </tr>
            <tr>
              <td>配套项目</td>
              <td>按范围</td>
              <td>待核定</td>
            </tr>
          </tbody>
        </table>
        <figcaption>报价排版示意 · 不含真实价格</figcaption>
      </figure>
    );
  }

  return null;
}
