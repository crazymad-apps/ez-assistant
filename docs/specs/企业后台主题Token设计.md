# 企业后台主题 Token 设计

本文档定义 `apps/enterprise-admin/` 的视觉 Token 体系。唯一代码来源是
`src/theme.ts`（Ant Design ConfigProvider 的 `ThemeConfig` 及全局语义样式配置），本文档与其保持一致；
修改视觉基准时先改 `theme.ts`，再同步本文档。

相关规范：[`企业后台前端开发规范.md`](企业后台前端开发规范.md)、
[`docs/modules/enterprise-admin.md`](../modules/enterprise-admin.md)。

## 一、定位与原则

- **唯一来源**：控件颜色、字号、圆角、控件高度一律由 ConfigProvider Token 派生；
  业务代码和 SCSS Module 不得硬编码与 Token 重复的视觉值。
- **不碰内部实现**：不覆写 AntD 内部 DOM 选择器、不引入第二个 CSS-in-JS；
  SCSS Module 只承担布局结构（壳层分栏、卡片排布、表格高度等）。
- **沿用原型基准**：Token 取自 C01 原型的视觉基线，调整 Token 属于设计变更，
  需要用户确认，不与功能改动混合提交。
- 这是管理后台专属的成套组件库试用决策；不影响 Desktop 的 SelectionPopover 等既有原语。

## 二、全局 Token（`token`）

### 品牌色

| Token | 值 | 用途 |
| --- | --- | --- |
| `colorPrimary` | `#2e81d8` | 品牌蓝：主按钮、链接、选中态、顶栏底色 |
| `colorPrimaryHover` | `#4a95e0` | 主色悬停 |
| `colorPrimaryActive` | `#1f6fc5` | 主色按下 |
| `colorInfo` | `#2e81d8` | 与主色一致，信息态不另立色相 |

### 文字

| Token | 值 | 用途 |
| --- | --- | --- |
| `colorText` | `#26303c` | 正文 |
| `colorTextSecondary` | `#687389` | 次要说明、表头文字 |
| `colorTextTertiary` | `#8a94a6` | 辅助/占位 |

### 边框与背景

| Token | 值 | 用途 |
| --- | --- | --- |
| `colorBorder` | `#d5dde8` | 控件边框 |
| `colorBorderSecondary` | `#e6ecf3` | 次级分隔 |
| `colorBgLayout` | `#eff4fa` | 内容区浅蓝灰底 |
| `colorBgContainer` | `#ffffff` | 卡片/弹窗白底 |
| `colorFillSecondary` | `#f2f5f9` | 浅填充（文本按钮悬停等） |
| `colorSplit` | `#edf1f6` | 表格分割线 |

### 状态色

| Token | 值 | 用途 |
| --- | --- | --- |
| `colorSuccess` | `#3fa66a` | 成功、启用态 |
| `colorWarning` | `#b7791f` | 警告 |
| `colorError` | `#d64545` | 失败、停用确认、校验错误 |

### 字号与排版

| Token | 值 | 说明 |
| --- | --- | --- |
| `fontSize` | 14 | 正文基准 |
| `fontSizeSM` | 12 | 辅助文字 |
| `fontSizeLG` | 16 | 大字号 |
| `fontSizeHeading1–4` | 16 | 标题层级压平为 16 |
| `fontSizeHeading5` | 14 | 最小标题 |
| `fontFamily` | 系统栈 | `-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", sans-serif` |

设计意图：管理后台信息密度优先，标题层级刻意压平（H1–H4 同为 16），
靠留白、字重和卡片分区表达层级，不靠字号跳跃。

### 圆角与控件高度

| Token | 值 | 用途 |
| --- | --- | --- |
| `borderRadius` | 6 | 控件基准圆角 |
| `borderRadiusLG` | 10 | 大容器圆角 |
| `controlHeight` | 32 | 输入框/按钮基准高度 |
| `controlHeightSM` | 24 | 紧凑控件 |
| `controlHeightLG` | 38 | 大尺寸控件 |

## 三、组件 Token（`components`）

整体骨架：**蓝色顶栏（48px）+ 白色侧栏 + 浅蓝灰内容区**。

| 组件 | 关键 Token | 设计意图 |
| --- | --- | --- |
| Layout | `headerBg #2e81d8`、`headerColor #fff`、`headerHeight 48`、`siderBg #fff`、`bodyBg #eff4fa` | 品牌蓝顶栏；侧栏与内容卡片同为白底，内容区铺浅蓝灰形成层次 |
| Menu | `itemHeight 40`、`itemBorderRadius 0`、`itemSelectedBg #edf3fd`、`itemSelectedColor #1f6fc5`、`activeBarBorderWidth 0` | 侧栏菜单去选中竖条、去圆角，用浅蓝整行底色表达选中 |
| Breadcrumb | `itemColor #687389`、`lastItemColor #26303c`、`separatorColor #98a2b3` | 当前页用正文色，路径用次要色 |
| Card | `borderRadiusLG 12`、`paddingLG 16`、`headerFontSize 16`、`headerHeight 52` | 页面主体卡片大圆角，标题与正文同字号 |
| Table | `headerBg #f5f8fc`、`headerColor #687389`、`headerSplitColor transparent`、`borderColor #edf1f6`、`rowHoverBg #f4f8fd`、单元格 padding 12/16 | 浅蓝表头、弱化分割线、行 hover 浅蓝反馈 |
| Button | `primaryShadow none`、`defaultBorderColor #d5dde8`、`textHoverBg #f2f5f9` | 去投影的扁平风格 |
| Input | `activeShadow none`、`hoverBorderColor #9db4cc`、`activeBorderColor #2e81d8` | 去聚焦光晕，仅靠边框色变化反馈 |
| Select | `activeOutlineColor transparent`、`optionSelectedBg #edf3fd` | 同上，选项选中与菜单选中同底 |
| Modal | `titleFontSize 16`、`contentBg #ffffff`；全局 `modal.styles.header.marginBottom 18` | 弹窗白底，标题 16；标题到正文间距与 Form.itemMarginBottom 共用 formItemSpacing=18 |
| Form | `labelColor #3f4a5f`、`itemMarginBottom 18` | 标签色略浅于正文 |
| Descriptions | `labelBg #f5f8fc` | 详情页标签列浅蓝底，与表头一致 |

## 四、使用与扩展规则

2026-09-20 用户指出 Modal 标题与内容间距不足：统一从默认 8px 调整为 18px。headerMarginBottom 不是公开组件 Token，因此使用 ConfigProvider 的 modal.styles.header 语义 API，不覆写内部选择器、不扩大 marginXS 对确认弹窗和按钮的影响；正式入口统一接入，原型保持历史基准。

1. 新页面/组件先查 `theme.ts`：能用现有 Token 表达就不写新值；布局间距、分栏、
   滚动容器等结构性样式才写 SCSS Module。
2. 确需新增视觉值时，优先加进 `theme.ts` 的全局或组件 Token（让全站生效），
   只有单一页面确实独有的值才允许留在该页面的 SCSS Module，并注释说明原因。
3. 禁止业务代码硬编码与 Token 相同的色值"图省事"——后续 Token 调整时应只改 `theme.ts` 一处。
4. 状态语义（成功/警告/失败）永远用状态 Token，不用品牌蓝或自造颜色表达。
5. Token 调整后按规范执行真实浏览器回归，重点看 125%/150% 缩放下的表格、弹窗与菜单。
