import type { RuntimeHostCapabilities } from "@ez-assistant/protocol";
import type { IconName } from "../../../../components/Icon";
import type { DesktopPlatform } from "../../../../native-bridge/desktopLifecycle";

export type AboutFeatureItem = Readonly<{
  id: string;
  icon: IconName;
  title: string;
  description: string;
}>;

export type ReleaseChangelogEntry = Readonly<{
  version: string;
  date: string;
  summary: string;
  items: readonly string[];
}>;

export type LicenseAttribution = Readonly<{
  name: string;
  license: string;
  usage: string;
}>;

export type EnvironmentSummaryInput = Readonly<{
  app_version: string;
  client_kind: "desktop" | "web";
  desktop_platform: DesktopPlatform;
  connected: boolean;
  capabilities: RuntimeHostCapabilities | null;
}>;

export const CURRENT_RELEASE_META = Object.freeze({
  version: "0.33.2",
  date: "2026-11-23",
});

/** 当前版本面向用户展示的核心 Feature 列表，与当前客户端交付能力保持同步。 */
export const CURRENT_VERSION_FEATURES: readonly AboutFeatureItem[] = [
  {
    id: "scheduled-tasks",
    icon: "clock",
    title: "定时任务与自动化",
    description:
      "支持创建 Agent 与脚本定时计划（一次、每日或每周执行），在计划详情查看执行记录与交付文件，并可交由主控会话汇报。",
  },
  {
    id: "background-runs",
    icon: "terminal",
    title: "后台命令运行与上下文分页",
    description:
      "Shell 命令可在后台持续运行，可在右侧「当前上下文」实时查看输出或停止进程；运行记录、技能、子任务、附件与产物支持分页查看。",
  },
  {
    id: "rich-appearance",
    icon: "palette",
    title: "外观主题与字体定制",
    description:
      "提供 15 套浅/深色配色主题、自定义强调色、全局字号、正文行高、阅读宽度、界面密度及内置 5 款阿里妈妈字体。",
  },
  {
    id: "device-direct-voice",
    icon: "device",
    title: "智能终端直连与语音交互",
    description:
      "支持局域网自动发现与免码一键配对、原生直连控制台与自动 MCP 桥接、自定义唤醒词、声纹直通录入，以及多配置 ASR / TTS 语音服务与在线试听。",
  },
  {
    id: "model-provider-setup",
    icon: "model",
    title: "模型服务商快捷配置",
    description:
      "全新分组服务商选择器与品牌图标，支持主流服务商一键预置、连接测试与自动拉取模型列表。",
  },
  {
    id: "controller-prompt-auth",
    icon: "shield",
    title: "助理提示词与精细授权",
    description:
      "支持助理会话专属提示词配置、隔离权限文档管理及一次性预授权。",
  },
];

/** 应用整体能力概览，供「App 功能」二级页展示。 */
export const APP_CAPABILITIES: readonly AboutFeatureItem[] = [
  {
    id: "sessions-workspaces",
    icon: "message",
    title: "多会话与工作空间",
    description:
      "支持工作空间多目录管理、主控会话与并行子 Agent、Plan / Build 模式切换、历史消息编辑、运行中介入与 Todo / Goal 长任务协作。",
  },
  {
    id: "models-context",
    icon: "model",
    title: "模型治理与上下文管理",
    description:
      "兼容 OpenAI / Responses 等主流模型接口，支持企业与个人模型分组切换、固定记忆、历史召回与自动上下文压缩。",
  },
  {
    id: "skills-mcp-plugins",
    icon: "puzzle",
    title: "Skills、MCP 与插件应用",
    description:
      "内置与用户级 Skill 灵活启停，支持 stdio / Streamable HTTP MCP 服务、消息交互卡片以及右侧资源栏插件应用。",
  },
  {
    id: "resource-workspace",
    icon: "folder",
    title: "资源工作区与内置终端",
    description:
      "在同一窗口浏览 Host 文件、代码、Markdown、图片与 PDF，集成内嵌浏览器与可交互 Host 终端。",
  },
  {
    id: "smart-devices",
    icon: "device",
    title: "智能终端与语音中继",
    description:
      "支持桌面与硬件语音终端协同，提供本地发现配对、声纹识别、ASR / TTS 语音链路和受控设备工具调用。",
  },
  {
    id: "local-and-network",
    icon: "globe",
    title: "本地优先与跨设备协同",
    description:
      "桌面端与独立 Runtime 双进程解耦，支持本机直连、Web 访问、企业多组织隔离、跨设备工作区与远程桌面控制。",
  },
];

/** 近期版本更新日志摘要，与 docs/site/changelog 保持口径一致。 */
export const RELEASE_CHANGELOG: readonly ReleaseChangelogEntry[] = [
  {
    version: "0.33.2",
    date: "2026-11-23",
    summary: "新增定时任务、后台命令运行管理、外观字库升级与智能终端直连能力。",
    items: [
      "新增定时任务，支持 Agent 与脚本两种计划类型，按一次、每日或每周执行并查看交付文件。",
      "Shell 命令支持后台持续运行，可在右侧「当前上下文」查看实时输出或停止进程。",
      "「当前上下文」的运行记录、技能、子任务、附件与产物支持分页查看。",
      "升级外观定制（15 套主题、自定义强调色、5 款阿里妈妈字体）、服务商分组快捷配置与智能终端直连/语音能力。",
    ],
  },
  {
    version: "0.33.1",
    date: "2026-11-17",
    summary: "改善长会话阅读定位、历史消息编辑、运行中介入与侧栏列表整理。",
    items: [
      "消息历史按完整轮次分页加载，宽屏左侧新增缩略导航并支持悬停预览与按需定位。",
      "用户消息支持空闲时编辑重发；运行中可通过 Ctrl / Command + Enter 或队列入口提前介入。",
      "项目、项目内会话与独立会话支持创建时间默认排序、拖拽重排与分页加载。",
      "修复取消或失败时的输出留存、压缩上下文重建及长文件按字符分段读取问题。",
    ],
  },
  {
    version: "0.33.0",
    date: "2026-11-09",
    summary: "新增多组织工作区、企业与个人多模型配置，以及插件应用与消息卡片。",
    items: [
      "支持同一账号加入并切换多个组织，会话、记忆、文件与个人模型按组织独立隔离。",
      "企业模式下可同时使用组织模型与个人服务商模型，列表分组清晰呈现。",
      "支持 MCP Apps 与轻量插件的消息交互卡片，右侧资源栏新增「应用」标签。",
    ],
  },
  {
    version: "0.32.1",
    date: "2026-10-27",
    summary: "优化桌面安装交接、长会话上下文性能、内置 MCP 与操作指引。",
    items: [
      "优化应用关闭与安装升级交接流程，自动处理旧客户端与本机后台收尾并保留原数据。",
      "改进长会话冷启动加载、本地与自动压缩流程，以及主会话与子任务用量快照。",
      "内置 DeepSeek 搜索与 skill-creator 资源，并提供离线按需读取的内置操作指引。",
    ],
  },
  {
    version: "0.32.0",
    date: "2026-10-19",
    summary: "新增企业组网设备管理、跨设备工作区与远程桌面查看控制。",
    items: [
      "支持设备登记、用户授权与桌面端切换授权设备，直接操作远端项目、会话、文件与终端。",
      "新增远程桌面查看与控制，支持多显示器选择、全屏与适应窗口模式。",
      "新增 OpenCode 服务商配置与工具参数 JSON 自动纠错反馈。",
    ],
  },
  {
    version: "0.31.0",
    date: "2026-10-14",
    summary: "推出外观主题切换、登录选项入口与模型异常调用证据排查。",
    items: [
      "新增「设置 → 外观」，侧栏、会话、代码块、编辑器与终端同步切换配色。",
      "登录首页支持切换个人或企业模式并配置企业中心地址。",
      "模型调用记录支持保留异常请求与响应正文，便于排查上游调用问题。",
    ],
  },
];

/** 随包核心开源组件与素材许可致谢。 */
export const LICENSE_ATTRIBUTIONS: readonly LicenseAttribution[] = [
  { name: "Tauri 2", license: "MIT / Apache-2.0", usage: "跨平台桌面应用容器与原生能力桥接" },
  { name: "React & MobX", license: "MIT", usage: "界面组件渲染与响应式状态管理" },
  { name: "Monaco Editor", license: "MIT", usage: "资源工作区只读代码与文本编辑器" },
  { name: "xterm.js", license: "MIT", usage: "内置 Host 终端仿真渲染" },
  { name: "Streamdown & Shiki", license: "Apache-2.0 / MIT", usage: "Markdown 排版、语法高亮、公式与 Mermaid 图表" },
  { name: "Three.js & Vanta", license: "MIT", usage: "启动与登录页动态点阵背景" },
  { name: "Material Icon Theme", license: "MIT", usage: "资源工作区文件与目录类型图标" },
  { name: "LobeHub Icons", license: "MIT", usage: "模型与语音服务商品牌图标" },
  { name: "阿里妈妈字库", license: "免费商用许可", usage: "内置方圆体、数黑体、灵动体、刀隶体与东方大楷" },
];

export function formatPlatformLabel(platform: string | null | undefined): string | null {
  switch (platform) {
    case "macos":
    case "darwin":
      return "macOS";
    case "windows":
    case "win32":
      return "Windows";
    case "linux":
      return "Linux";
    default:
      return null;
  }
}

export function formatArchitectureLabel(architecture: string | null | undefined): string | null {
  if (!architecture) return null;
  if (architecture === "x86_64") return "x64";
  if (architecture === "aarch64") return "arm64";
  return architecture;
}

export function buildEnvironmentSummary(input: EnvironmentSummaryInput): string {
  const { app_version, client_kind, desktop_platform, connected, capabilities } = input;
  const lines: string[] = [
    "应用: EZ Assistant",
    `客户端: ${client_kind === "desktop" ? "Desktop" : "Web"}`,
    `客户端版本: v${app_version}`,
  ];

  const client_os = client_kind === "desktop" ? formatPlatformLabel(desktop_platform) : null;
  if (client_os) {
    lines.push(`客户端系统: ${client_os}`);
  }

  if (capabilities?.runtime_version) {
    const status_label = connected ? "已连接" : "已缓存";
    lines.push(`Runtime 版本: v${capabilities.runtime_version} (${status_label})`);
  } else {
    lines.push("Runtime 版本: 未连接");
  }

  if (capabilities?.min_compatible_version) {
    lines.push(`最低兼容版本: v${capabilities.min_compatible_version}`);
  }

  if (capabilities?.mode) {
    lines.push(`运行模式: ${capabilities.mode === "enterprise" ? "企业模式" : "个人模式"}`);
  }

  const host_os = formatPlatformLabel(capabilities?.platform);
  const host_arch = formatArchitectureLabel(capabilities?.architecture);
  if (host_os || host_arch) {
    lines.push(`Host 平台: ${[host_os, host_arch].filter(Boolean).join(" · ")}`);
  }

  return lines.join("\n");
}
