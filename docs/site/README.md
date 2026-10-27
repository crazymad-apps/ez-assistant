# 官网内容维护

首页与手册使用仓库中的配置、Markdown 和图片；更新日志按软件版本单独维护，面向用户简明说明。
内容维护与平台源码构建解耦，官网仍在现有企业中心前端工程中。

v0.31.0 F01 已实现内容维护、正式页面与 Docker 同端口入口。本目录包含首页和当前手册初始内容；
更新日志仍为未发布草稿，只有安装版本实际发布后才对外展示。Demo 保留模拟内容，未与真实发布数据绑定。

## 三类数据分别维护

| 内容 | 维护方式 | 公开来源 |
| --- | --- | --- |
| 客户端下载 | 在安装包管理上传、发布或撤回 | 自动按平台选最新已发布、文件可用且兼容的安装包，无须再选官网版本 |
| 更新日志 | 仓库按版本维护；版本详情直接编辑并保存 Markdown 正文 | 安装版本的现有更新说明字段，官网与客户端共用 |
| 首页与操作手册 | 仓库修改，校验／预览后更新独立固定静态目录 | `/site-content/`，与安装包版本无关 |

## 更新日志

源文件为 `changelog/vX.Y.Z.md`，保留版本与日期元信息。通常从文件复制正文到对应版本详情的
“更新日志”编辑框，预览后保存，不复制 YAML 头部。允许在平台直接纠错，之后将相同修订回存仓库，
保持仓库的权威静态数据。系统不自动拉 Git 或回写仓库。

官网版本与发布日期以本中心版本身份及最早首次发布时间为准，不以正文日期决定是否公开。
未发布版本的说明只供后台编辑；至少一个平台首次发布后公开，同版多平台只展示一条。
全部平台撤回后日志继续保留，安装包不可下载。已发布版本保存说明后官网和客户端读取新正文。
旧版本已有说明直接复用，空说明不伪造日志。正文保持现有 16 KiB UTF-8 上限。

只写用户可感知的新增、改进和修复，必要时补升级提醒。不要粘贴开发进度、Feature 编号、测试流水账
或尚未实现的承诺；完整约定见[开发流程规范](../specs/开发流程规范.md#用户向更新日志与操作手册)。

## 首页与手册

源目录采用 `site.json`、`home/`、`manual/` 与 `assets/`。静态清单只选择首页区块、当前手册目录和
所需图片，不包含安装版本、最新下载 URL 或日志集合。手册按主题维护一套当前内容，文章可注明适用版本。
仓库中未列入公开清单的草稿不输出，日志源文件也不进入手册静态目录。

维护步骤：

1. 修改源文件，核对实际已交付功能及图片。
2. 校验并本地预览，输出受限 `content.json` 与公开图片；不生成安装版本绑定的 ZIP。
3. 更新配置指定的独立静态目录：先准备哈希图片，再原子替换内容 JSON；失败不切换入口。
4. 刷新官网确认。无需安装包操作、应用重建、服务重启或后台重载。

对外固定读取 `/site-content/content.json`，图片使用内容哈希路径。常规更新保留旧公开图片，以供
已打开页面继续读取；清理旧图片是另行核对的运维动作。回退时重新部署仓库中确认正确的内容。

部署配置 `CENTER_SITE_CONTENT_ROOT` 指向专用公开目录，Docker 入口只读挂载。不得指向整个仓库、
安装包私有目录、模型快照或账号配置。禁止目录列表、点文件与任意路径访问；此目录中的文件均视为
可公开数据，不能把“没在页面上链接”当成保密措施。

支持常用 Markdown，禁止原始 HTML、脚本、嵌入对象；图片只接纳经检查的 PNG、JPEG、WebP。
资源、链接、容量及交付约定见 [F01 技术方案](../versions/v0.31.0/features/F01-企业中心官网与公开页面/技术方案.md)。
固定目录方案首版不增加独立上传界面，更不在安装包管理里增加手册操作。

## 格式与实际命令

静态源清单格式示例（完整初始内容见本目录 `site.json`）：

```json
{
  "format": 1,
  "home": [{"kind": "hero", "title": "首页标题", "description": "首页简介", "file": "home/hero.md"}],
  "manual": [{"title": "开始使用", "articles": [{"id": "installation", "file": "manual/installation.md"}]}],
  "assets": []
}
```

上述字段必需，未使用的集合填空数组；图片必须由选定的正文引用。首页正文不带 YAML 头部；手册示例：

```yaml
---
id: installation
title: 安装客户端
updated: '2026-10-13'
applies_to: '0.30.0'
---
```

日期与适用版本按实际内容填写。稳定段落可写 `## 安装步骤 {#install}`，站内文章使用相对 Markdown
路径或 `/manual/installation#install`，日志链接使用 `/changelog#v0.30.0`。
图片使用 `../assets/installation.png` 等相对路径，并在 assets 中登记 id 与 file；不能填远程图片。

初次准备维护工具时，在仓库根构建一次 Center；此步骤安装／更新工具使用，日常内容发布不再编译应用：

```bash
npm --prefix apps/enterprise-center run build
```

生成到一个不存在的预览目录（父目录须存在）：

```bash
npm --prefix apps/enterprise-center run site:build -- ../../docs/site /absolute/path/to/new-preview
SITE_CONTENT_PREVIEW_DIR=/absolute/path/to/new-preview npm --prefix apps/enterprise-admin run dev:content-preview
```

本地预览地址为 `http://127.0.0.1:7313/`，只监听本机，使用正式首页／手册完整布局，站内链接可以导航，顶部标明本地内容预览；不读取中心日志或执行真实下载。
更新源后可用 `site:publish` 更新同一预览输出目录，再刷新页面；未发布日志不进入静态清单或输出。

发布到明确指定的专用静态目录：

```bash
npm --prefix apps/enterprise-center run site:publish -- ../../docs/site /absolute/path/to/public-content
```

该命令直接校验源文件，原子更新图片和最后的 content.json；可创建或更新目标目录，不连接数据库。
输出文件为静态数据而非 ZIP。目标目录是公开数据区域，必须与源目录分离；不可指向平台程序、
仓库、安装包或快照目录。Center 设置 `CENTER_SITE_CONTENT_ROOT=/absolute/path/to/public-content`
后在 `/site-content/` 只读提供内容；首次配置需按原启动流程启动，后续更新内容无需重启。
文件采用目录 0755、文件 0644，便于只读服务进程读取；部署者仍应限制写入权限。


## 首页呈现约定

M1 六类区块按清单顺序渲染：`hero`、`workspace`、`capabilities`、`modes`、`resources`、`closing`。
省略的区块不显示；标题、简介和正文均从静态目录读取。品牌、导航与平台选择提示属于界面文本。
`capabilities` 正文按二级标题分成卡片，`modes` 正文可写个人／企业模式对比表。

`workspace` 可以附加 `scenarios`，用于保留已确认的客户端布局演示；它是静态演示，不执行插件。
每个场景字段为 `id`、`label`、`plugin`、`prompt`、`intro`、`title`、`body`、`file`、`reply`、
`steps`（文字数组）和 `kind`（`drawing`、`scene`、`quote`、`notes`）。最多八个场景，ID 唯一；
只接收受限文字，不接收代码、组件或 HTML。场景文字随内容更新，无需重新构建网页。
能力卡片标题使用 `{#场景ID}` 关联对应示意图，例如 `## 布局绘图 {#cad}`；顺序调整不会错配图形。

首页、手册和下载／日志互相独立：安装版本为空不妨碍读手册，静态内容缺失不妨碍查询下载。
内容目录缺失时保持空态；不以原型日志、假文件或测试手册兜底。首页工作台与交付图形明确标为示意，
只呈现插件能力，不执行真实任务。配图沿用已确认的代码示意图，不复制客户图纸或客户端中的私有资料。

Docker 部署将 `site:publish` 输出目录只读挂载到入口的 `/srv/site-content`，详见
[部署说明](../../apps/enterprise-center/deploy/README.md)。内容工具运行在维护端，网页不直接读取仓库。

## 插件使用文档的维护

手册“专业插件”分组包含安装总览、[插件开发规则](manual/plugin-development.md)与五个独立使用章节；
`plugins.md` 的原有段落锚点继续保留。开发规则核对 Host 与中心实际准入校验，以及独立插件仓库的
`docs/development.md`、`docs/plugin-contract.md`，区分源码目录、归档结构和安装后的可写目录。
该页也作为规划中的内置操作手册 Skill 的插件开发内容来源；Skill 的资源装配与生效方式由对应
Feature 实现，不因官网正文存在就视为内置能力已可用。
插件能力、版本和运行前提以独立 `ez-assistant-plugins` 仓库对应目录的 `plugin.json`、
`README.md` 和 Skill 使用规则为依据，转写成用户操作说明后在此维护，不自动将源码说明全部公开。

| 插件目录 | 使用文档 |
| --- | --- |
| warehouse-cad-guidance | [库房 CAD 绘图](manual/warehouse-cad.md) |
| cad-scene-preview | [CAD 图纸转三维](manual/cad-scene.md) |
| quotation-assistant | [生成 Excel 报价](manual/quotation.md) |
| gorag-knowledge | [查询企业知识库](manual/gorag-knowledge.md) |
| general-assistant | [创建可复用技能](manual/general-assistant.md) |

插件升级后同步核对各篇适用版本、输入要求、交付内容、示例与常见问题，并更新总览版本表。
不把仓库版本当作已上架版本，不公开源资料里的私有服务地址、凭据、客户案例或开发机路径。
