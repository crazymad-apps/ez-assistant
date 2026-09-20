import type { ConfigProviderProps, ThemeConfig } from 'antd';

const formItemSpacing = 18;

// headerMarginBottom 是库内部 Token；通过公开语义样式只调整标题间距，不扩大 marginXS 影响按钮。
export const adminModalConfig: ConfigProviderProps['modal'] = {
  styles: { header: { marginBottom: formItemSpacing } },
};

/** Ant Design 是控件视觉的唯一 Token 来源；布局骨架同样通过组件 Token 定制，避免覆写内部 DOM。 */
export const adminTheme: ThemeConfig = {
  token: {
    colorPrimary: '#2e81d8',
    colorPrimaryHover: '#4a95e0',
    colorPrimaryActive: '#1f6fc5',
    colorInfo: '#2e81d8',
    colorText: '#26303c',
    colorTextSecondary: '#687389',
    colorTextTertiary: '#8a94a6',
    colorBorder: '#d5dde8',
    colorBorderSecondary: '#e6ecf3',
    colorBgLayout: '#eff4fa',
    colorBgContainer: '#ffffff',
    colorFillSecondary: '#f2f5f9',
    colorSplit: '#edf1f6',
    colorSuccess: '#3fa66a',
    colorWarning: '#b7791f',
    colorError: '#d64545',
    borderRadius: 6,
    borderRadiusLG: 10,
    fontSize: 14,
    fontSizeSM: 12,
    fontSizeLG: 16,
    fontSizeHeading1: 16,
    fontSizeHeading2: 16,
    fontSizeHeading3: 16,
    fontSizeHeading4: 16,
    fontSizeHeading5: 14,
    controlHeight: 32,
    controlHeightSM: 24,
    controlHeightLG: 38,
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif',
  },
  components: {
    // 整体骨架：蓝色顶栏 + 白色侧栏 + 浅蓝灰内容区。
    Layout: {
      headerBg: '#2e81d8',
      headerColor: '#ffffff',
      headerHeight: 48,
      headerPadding: '0 16px',
      siderBg: '#ffffff',
      bodyBg: '#eff4fa',
    },
    Menu: {
      itemHeight: 40,
      itemBorderRadius: 0,
      itemSelectedBg: '#edf3fd',
      itemSelectedColor: '#1f6fc5',
      itemColor: '#3f4a5f',
      activeBarBorderWidth: 0,
    },
    Breadcrumb: {
      itemColor: '#687389',
      lastItemColor: '#26303c',
      separatorColor: '#98a2b3',
    },
    Card: {
      borderRadiusLG: 12,
      paddingLG: 16,
      headerFontSize: 16,
      headerHeight: 52,
    },
    Table: {
      headerBg: '#f5f8fc',
      headerColor: '#687389',
      headerSplitColor: 'transparent',
      borderColor: '#edf1f6',
      rowHoverBg: '#f4f8fd',
      cellPaddingBlock: 12,
      cellPaddingInline: 16,
    },
    Button: {
      primaryShadow: 'none',
      defaultBorderColor: '#d5dde8',
      textHoverBg: '#f2f5f9',
    },
    Input: {
      activeShadow: 'none',
      hoverBorderColor: '#9db4cc',
      activeBorderColor: '#2e81d8',
    },
    Select: {
      activeOutlineColor: 'transparent',
      optionSelectedBg: '#edf3fd',
    },
    Modal: {
      titleFontSize: 16,
      contentBg: '#ffffff',
    },
    Form: {
      labelColor: '#3f4a5f',
      itemMarginBottom: formItemSpacing,
    },
    Descriptions: {
      labelBg: '#f5f8fc',
    },
  },
};
