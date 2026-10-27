---
id: plugin-development
title: 插件开发规则
updated: '2026-10-21'
applies_to: 'Runtime 0.28.0+'
---

本页面向插件开发者，说明 EZ Assistant 插件的目录、文件和打包规则。
安装已有插件请阅读[安装与使用插件](plugins.md)。插件使用 EZ Assistant 的 `format_version: 1`
清单；开发、安装和执行都以当前连接的 Runtime（Host）及其平台为准。

## 源码目录与安装包 {#structure}

每个插件使用独立的同名源码目录，独立维护版本、依赖和打包入口。业务插件在独立的
`ez-assistant-plugins` 仓库维护，通过插件包与 Host 集成。

下面是源码目录示意。按能力保留所需文件，纯 Skill 插件无需创建 MCP 和运行时目录：

```text
example-tools/
├── plugin.json                  # 插件身份、版本、平台和能力清单
├── README.md                    # 开发、构建、使用与限制说明
├── skills/
│   └── example-guide/
│       ├── SKILL.md             # 技能入口与操作规则
│       ├── references/          # 按需读取的参考资料
│       ├── scripts/             # 技能使用的脚本
│       └── assets/              # 技能使用的模板和素材
├── mcp/
│   └── example-server/          # MCP 实现及描述符，描述符也可在打包时生成
├── resources/                   # 插件共享的只读资料
├── licenses/                    # 随包依赖与素材的许可说明
├── scripts/
│   └── package.py               # 现有插件仓库的打包入口
├── tests/                       # 与本插件有关的测试
└── dist/                        # 生成的包与校验和，不手改、不提交
```

依赖声明与锁文件放在所属插件目录。构建脚本从自身位置定位源码，不依赖主应用源码、另一个插件
或开发机固定路径。运行时文件可由打包脚本从显式输入复制，不要求把运行时副本提交到源码仓库。

ZIP 或 TAR.GZ **归档根必须直接包含 `plugin.json`**，不能再套一层 `example-tools/`。
安装包的根层只允许以下文件与目录，未使用的目录可以省略：

```text
plugin.json
skills/<skill-name>/SKILL.md
mcp/<server-name>/server.json
mcp/<server-name>/server.<os>-<arch>.json
runtimes/<os>-<arch>/
resources/
licenses/
```

两种 MCP 描述符按需选用；平台专用文件优先于通用 `server.json`。每个声明的平台都必须有可用入口。
根层 `README.md`、`scripts/`、`tests/`、依赖清单及 `.git/` 不进入安装包；随包说明放到
`resources/README.md`，技能脚本可保留在 `skills/<skill-name>/scripts/` 中，MCP 代码和所需依赖
放入相应的 `mcp/` 或 `runtimes/` 子目录。

## 清单与命名 {#manifest}

这是一个仅包含 Skill 的完整最小清单示例，平台声明应按实际支持范围调整：

```json
{
  "format_version": 1,
  "name": "example-tools",
  "display_name": "示例工具",
  "description": "提供示例任务的操作指引。",
  "version": "1.0.0",
  "platforms": [{ "os": "macos", "arch": "arm64" }],
  "min_host_version": "0.28.0",
  "skills": ["example-guide"],
  "mcp": [],
  "configuration": [],
  "external_software": []
}
```

- `name` 是稳定插件标识，与源码目录名一致；使用小写英文字母、数字及单个连字符分段，
  不以连字符开头或结尾，不含连续连字符，最长 64 字符。Skill 名称遵循相同规则。
- `display_name` 和 `description` 用于展示；修改展示名称不等于修改插件标识。
- `version` 和 `min_host_version` 使用三段非负整数，例如 `1.0.0` 和 `0.28.0`，
  不带 `v`、预发布后缀或构建后缀。前者是插件版本，后者是所需最低 Host 版本。
- `platforms` 中 `os` 可为 `macos`、`windows`、`linux`，`arch` 可为 `x64`、`arm64`。
  这里只说明清单格式支持的值，实际支持范围由插件内容与目标平台验证确定。
- `skills`、`mcp` 填能力名称，不填路径；每组名称不重复，至少声明一个 Skill 或 MCP。
  每项都必须有对应目录与入口，未声明的 Skill／MCP 目录不打入包。
- MCP 服务名以小写英文字母开头，后续可用小写英文字母、数字、点、下划线和连字符，最长 64 字符。
- `configuration` 声明用户参数，每项包含 `name`、`label`、`required`、`secret`。
  `external_software` 声明外部软件，每项包含 `name`、`display_name`、`instructions`，
  可补充 `download_url` 和适用 `platforms`。无需配置或外部软件时填空数组。

清单使用合法 JSON，不含注释、未知字段或真实凭据。不要套用其他平台的清单路径、字段或安装钩子。

## Skill 文件 {#skills}

每个技能必须提供大小写准确的 `skills/<skill-name>/SKILL.md`，文件使用 UTF-8，开头是 YAML 头部：

```markdown
---
name: example-guide
description: 当用户需要完成示例任务时，提供输入检查、执行步骤和结果核对方法。
---

# 示例任务

先核对用户提供的资料和目标，再按需读取 references/ 中的说明。
执行后检查交付文件，说明结果及仍需确认的事项。
```

`name` 必须与目录名及清单声明一致；`description` 非空，最长 1024 字符，应写清适用任务。
正文说明输入、操作顺序、工具前提、输出和核验方式。长参考资料放在 `references/`，
模板放在 `assets/`，脚本放在 `scripts/`，从技能目录按相对路径引用，避免把所有资料塞进入口正文。

Skill 不能把未提供的工具写成可调用能力，也不能通过操作说明扩大权限。创建个人可复用技能时，
使用当前运行环境提供的用户 Skill 目录；修改插件技能时应修改源码、重新打包安装，不能直接改已安装包。

## MCP 描述符与运行时 {#mcp}

清单 `mcp` 中的每个名称对应 `mcp/<server-name>/`。描述符顶层只包含 `mcpServers`，
其中只定义一个与该目录及清单同名的服务。例如 `mcp/example-server/server.macos-arm64.json`：

```json
{
  "mcpServers": {
    "example-server": {
      "command": "${plugin_root}/runtimes/macos-arm64/example-server",
      "args": [],
      "cwd": "${plugin_root}/mcp/example-server"
    }
  }
}
```

该示例要求包内实际存在对应可执行文件；macOS／Linux 入口需要可执行权限。
Windows 的描述符和运行时分别放在 `server.windows-x64.json`、`runtimes/windows-x64/` 等对应位置。
同时提供通用与平台描述符时，两者都须合法，不能用专用文件掩盖无效的通用配置。

- 本地 stdio 服务使用 `command`，入口必须指向 `${plugin_root}/...` 下的包内文件，
  或指向已声明外部软件的 `${software.<name>}`；不依赖用户机器上碰巧存在的 Python／Node。
- 远程服务使用 `url` 和按需配置的 `headers`，与本地 `command` 配置二选一。
  远程地址使用 HTTPS；HTTP 只用于 loopback 本机服务。
- `${plugin_root}` 用于包内路径；`cwd` 若填写，须指向 `${plugin_root}/` 下实际存在的子目录。
  `${config.<name>}` 引用清单已声明的用户参数，只可用于 `env`、`headers`；
  `${software.<name>}` 引用已声明的软件路径，
  可用于 `command`、`args`、`env`。不要自造占位符或硬编码个人目录。
- 本地服务的 stdout 只输出 MCP 协议，日志与诊断写入 stderr。工具名、输入 schema、返回值及
  错误语义与 Skill 和示例保持一致，变更时核对已有调用方。
- 程序、解释器和依赖随目标平台包准备好，安装或运行时不隐式下载依赖。CAD 等外部软件
  通过清单声明并由用户配置，不能把“插件安装成功”当作外部软件已可用。

## 安装后的目录与写入规则 {#runtime-directories}

以下路径相对当前 Host 的**用户数据目录**，不是固定的系统 Home。企业用户域和远程连接应以
运行环境提供的实际目录为准，不根据客户端位置猜测：

```text
plugins/<plugin-name>/
├── packages/<sha256>/           # Host 管理的包内容，按只读使用
├── config/                     # 插件自定义配置（mcp-overrides.json 由 Host 管理）
└── data/                       # 插件运行数据、缓存和临时文件
```

Host 为插件本地 MCP 注入以下环境变量，插件不得在描述符的 `env` 中覆盖它们：

| 环境变量 | 用途与写入规则 |
| --- | --- |
| `EZ_ASSISTANT_PLUGIN_ROOT` | 当前生效包根目录；只读资源，不写入配置、日志、缓存或输出 |
| `EZ_ASSISTANT_PLUGIN_CONFIG_DIR` | 自定义配置文件；与清单声明的用户参数分开，按实际配置说明维护 |
| `EZ_ASSISTANT_PLUGIN_DATA_DIR` | 运行数据、缓存和临时脚本；不作为用户最终交付目录 |

这些变量用于本地 MCP 进程，不等于 Skill 中可直接展开的模板变量。技能脚本需要路径时，
通过实际执行环境或明确参数传递，不自行拼接另一用户或另一插件的目录。

用户要求的图纸、表格、预览和报告写入其指定或已授权的项目／交付目录。
不写回 `packages/`，不直接修改 Host 数据库或安装状态，也不访问其他插件的私有目录。
目录职责与工作目录不构成操作系统强沙盒，文件和 Shell 操作仍遵循实际权限与用户授权。

## 打包、安装与核验 {#verify}

1. 同步修改清单、Skill、MCP、README 和依赖锁文件；包内容与当前版本声明一致。
   使用独立三段版本号，已分发版本修复后升版本，不静默替换同名同版本的包。
2. 在插件源码目录运行 `python3 scripts/package.py --help` 查看已有打包入口，
   按该插件 README 提供所需平台运行时和参数；不同插件的参数不能直接混用。
3. 检查归档根、声明入口、运行时、可执行权限和许可证。包内路径使用 `/`，
   不含绝对路径、`..`、反斜杠、重复路径或大小写冲突；面向 Windows 的包避免保留文件名，
   发布包使用实体文件，避免符号链接。只打包所需内容，不包含凭据、用户数据、日志或开发缓存。
4. 将包移出源码目录解包检查，确认无需借用源码和构建机依赖；生成并核对 SHA-256。
   同一插件名和版本使用一份覆盖已声明平台的包，不用不同平台单包相互覆盖发布。
5. 在新建的隔离 Runtime 用户数据目录安装，检查包版本、平台、启用状态和配置提示，
   再核对 Skill 发现、MCP 连接及实际任务结果。正式用户目录不用于测试清理或重建。
6. 按实际范围说明哪些平台、功能和外部软件已验证。包通过静态校验、MCP 可连接和
   业务结果正确是不同结论；修改后按当前刷新规则或新建会话核验，不宣称历史会话已自动更新。

常见拒收原因是归档多套了一层目录、包根混入 README 或构建文件、清单名称与目录不一致、
缺少声明平台的 MCP 入口，或运行时路径和权限不正确。先核对包内容，再处理连接与业务问题。
