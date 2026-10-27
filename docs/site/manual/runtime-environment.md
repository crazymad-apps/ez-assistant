---
id: runtime-environment
title: 运行环境与目录
updated: '2026-10-23'
applies_to: 'Runtime 0.32.1+'
---

EZ Assistant 的桌面、Web 和 Client 是访问入口；Agent、会话及工具由连接的 Runtime Host 执行。企业中心管理账号、资源与分发，不代表每次文件操作都发生在企业中心或当前客户端设备上。

## 模式与执行目标 {#identity}

新会话由 Host 明确提供 `runtime_mode`：`personal` 是个人用户域，`enterprise` 是企业用户域。企业用户也可按实际来源管理个人配置；模式不是工具授权，不代表企业模式下所有设置只读。

远程连接时，文件、依赖、网络和 Shell 都以目标 Host 为准。客户端本机路径不能直接当作远端路径；切换连接或账号后，要核对正在访问的用户域及会话，不把旧路径转用于另一台设备。不要根据模型、中心地址、操作系统用户名或目录名猜模式。

## 环境提供的目录 {#directories}

| 事实 | 用途 |
| --- | --- |
| `user_data_directory` | 当前用户域私有根，不是操作系统 Home 或项目根 |
| `user_skills_directory` | 未指定位置时，用户可复用 Skill 的默认根 |
| `working_directory` 与附加工作区 | 相对文件路径、Shell cwd 与项目上下文；附加根使用明确绝对路径 |
| Workspace 私有目录 | Host 管理的工作区内容，区别于用户项目源码 |
| 会话私有目录 | 该会话的工作文件；不是正式 Skill 的默认安装目录 |
| 附件与工具图片目录 | 由 Host 管理的会话资源，不作为插件或 Skill 安装根 |

创建用户技能使用 `user_skills_directory/<name>/SKILL.md`；项目专用技能可按用户选择放在工作区 `.ez-assistant/skills/<name>/SKILL.md`。用环境提供的完整路径，缺少事实时确认目标，不自行拼用户名或企业目录。详见[创建可复用技能](general-assistant.md)。

插件包和内置资源是只读来源。插件的配置／数据写入各自可写目录，不直接改包内文件或 Host 数据库，详见[插件开发规则](plugin-development.md)。工作目录和应用权限都不等于操作系统强沙盒，Shell 仍以当前用户权限运行。

## 新旧会话 {#sessions}

恢复会话会保留冻结的系统提示词和历史 Skill 正文；升级不自动补写旧会话缺少的模式。新建会话取得当前环境；用户显式清空历史会沿原流程重新生成环境，但不把工作区后续更改的目录热更新进旧会话。

Fork 保留来源的冻结工作区和历史，重建新会话私有目录与当前 Host 环境事实。异常或无法识别的环境片段会明确失败，不猜测或丢弃未知指令。跨 Host／跨用户域导入不属于当前支持入口，不能靠复制提示词完成身份切换。

Skill／MCP 的发现及后续执行生效范围分别遵循[Skill 使用](general-assistant.md)和[MCP 配置](mcp-configuration.md)。读取手册只提供说明，实际操作与结果仍由工具及权限核验。
