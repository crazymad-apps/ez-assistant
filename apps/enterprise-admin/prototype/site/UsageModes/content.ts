export const modeComparison = [
  { label: '开始使用', personal: '进入个人工作环境，自行完成配置。', enterprise: '连接企业中心，使用企业账号登录。' },
  {
    label: '模型服务',
    personal: '自行配置服务商、密钥与模型。',
    enterprise: '使用中心管理的模型与代理服务，无需逐人配置服务商密钥。',
  },
  {
    label: '专业插件',
    personal: '上传插件包，自行安装与维护。',
    enterprise: '从企业插件市场按需安装，获取管理员发布的兼容版本。',
  },
  { label: '智能体', personal: '自行创建和编辑本地智能体。', enterprise: '使用管理员统一维护的智能体与能力配置。' },
  { label: '适合谁', personal: '希望自由搭配工具的个人用户。', enterprise: '希望统一配置、分发专业能力的团队。' },
] as const;
