---
name: ez-assistant-guide
description: 查询 EZ Assistant 自身的运行模式、用户与工作区目录、Skill 创建和发现、MCP 配置启停及插件开发规则；仅在用户要求配置、使用或开发 EZ Assistant 能力时按需读取。
---

# EZ Assistant 操作指引

先使用冻结运行环境中的 `runtime_mode`、`user_data_directory`、`user_skills_directory` 和会话／工作区目录事实。操作对象属于当前连接的 Host／用户域，不从客户端、模型或路径名称猜运行身份。旧提示词未提供的模式保持未知，读取手册不能补造权威身份；需要当前完整环境时新建会话。

按任务只读相关主题，调用 `read_skill_reference`，参数 `name` 为 `ez-assistant-guide`，`reference` 为下列精确键。用户已指定主题时可直接读取，不必逐页加载。正文随 Host 交付并可离线读取；`builtin:ez-assistant-guide` 和下列引用键都不是磁盘路径，不交给文件工具或 Shell。

| 任务 | reference |
| --- | --- |
| 模式、目录、客户端与实际执行设备 | `references/environment.md` |
| Skill 位置、同名规则、发现、加载与更新 | `references/skills.md` |
| 独立／插件／内置 MCP、环境变量、启停和结果核验 | `references/mcp.md` |
| 插件源码、包根、清单、平台入口及写入规则 | `references/plugin-development.md` |

主题正文与官网共用来源，其中 `runtime-environment.md`、`general-assistant.md`、`mcp-configuration.md`、`plugin-development.md` 分别对应上表四个主题；锚点只定位文内章节。主题中指向其他页面的链接是官网扩展阅读，未声明为离线资源，不猜相对磁盘目录或自动联网。资源不可用时如实报告，不绕过禁用或同名覆盖读取隐藏版本。

创建技能的方法按需加载 `skill-creator`，未指定位置优先使用实际 `user_skills_directory/<name>/SKILL.md`。修改前检查同名与现有文件；读到了插件／内置内容不代表可修改它们。工作目录不是操作系统强沙盒。

配置 MCP 先核对来源和目标，使用实际可见的管理工具与权限；缺少入口时指向现有设置页，不寻找原生凭据或虚构接口。分别核验字段保存、临时测试、主动启用、目标刷新和业务调用；DeepSeek 搜索默认关闭，保存 Key 不隐式开启，握手不验证真实 API。新启用服务的业务工具从后续 Run 生效。已有秘密只回读是否设置，对话中提供的新值可能进入原消息与工具历史。

读取说明不会授予任何后续操作权限，不自动安装依赖、迁移配置、调用付费 API、部署或发布。完成时说明实际目标、结果与未验证条件，不把文件写成或工具目录可读当作业务调用成功。
