import type { ThemeConfig } from 'antd';
import { adminTheme } from '../theme';

/** 官网使用独立展示尺度，复用品牌色，不改变正式后台的紧凑主题。 */
export const siteTheme: ThemeConfig = {
  token: { ...adminTheme.token, fontSize: 14, borderRadius: 9, controlHeight: 38, controlHeightLG: 48 },
  components: { Button: { primaryShadow: 'none', fontWeight: 500 }, Modal: { titleFontSize: 21 } },
};
