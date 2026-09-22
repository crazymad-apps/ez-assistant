# C03 技术方案附件 C：配置与版本升级

本附件属于[技术方案](技术方案.md)，已于 2026-09-20 随主方案获用户确认，M0 配置与布局升级已实现并获用户确认。采用用户要求的启动时版本分派；验证只使用隔离数据，没有操作用户既有数据目录。

## 一、Host 与用户配置

### 1.1 host.toml

最小个人配置示意：

```toml
version = "0.27.0"
mode = "personal"

[host_access]
remote_enabled = false
scheme = "http"
port = 7240
server_names = []
```

企业模式增加：

```toml
mode = "enterprise" # 顶层字段，替换上例的 mode

[enterprise]
center_url = "https://center.example.com"
# center_id = "01234567-89ab-4cde-8f01-23456789abcd"
# 首次成功认证后由 Host 写入；这里仅展示字段格式。
```

两个代码块是字段示意，不直接拼接；`mode` 始终只有一个且位于顶层。`center_id` 使用 Center 已持久化的 UUID，不是地址。地址支持变化，绑定的身份不因此变化；新的中心须由本机管理显式清除旧绑定并重启，原用户目录保留。

`host_access` 复用当前字段，包括可选的 `password_hash`、`tls_certificate`、`tls_private_key`；密码只用于个人访问，企业身份由 Center 验证。网络访问开关、来源校验、TLS 与已有远程启用前置条件继续执行，不因企业模式自动开放监听。

Host 设置经本机 bootstrap 管理。模式/中心/监听变更保存后显示 `restart_required`，当前实例继续使用启动时冻结的模式、中心和监听配置，重启后整体生效；不能新请求指向新 Center，旧任务仍指向旧 Center。首次成功认证绑定 center_id 是当前中心下的受控写入，必须在发布登录前完成。

### 1.2 配置源拆分

| 当前内容 | 新位置与使用者 |
| --- | --- |
| `config.toml` 的 `[host_access]` | `host.toml`；仅 Host access/监听管理读取 |
| 新模式、中心地址/身份、布局版本 | `host.toml`；Host 启动和身份接入读取 |
| Runtime/Agent/MCP 运行参数、`schema_version = 1` 等其余配置 | `users/<key>/config.toml`；本人 Runtime；Provider/模型管理记录继续在本人的 SQLite，不迁回配置文件 |
| `[speech]` | 各用户 `config.toml`；同一 Speech 服务读取本人配置。旧配置迁入 `_personal`，不复制给新用户 |
| `mcp.json`、权限、Recall 引用密钥 | 各用户域原有相对位置；不并入 host.toml |

复用 `LocalConfigSource` 的文件安全检查、大小限制、原子替换、内容摘要 CAS 和 write_gate；每个物理文件只有一套共享源实例。Host 写入不再调用 `refresh_configuration_projection` 去刷新某个 Runtime；用户配置写入只更新该用户投影。编辑源码中的配置注释/模块说明也须同步所有权变化。

Client 的 status/config/start/web 通过 Host 协议或既有 Host 离线 JSON IPC 获取配置，CLI 不自行增加 TOML 解析器。Desktop 原生发现、`--runtime-home` 和环境覆盖仍指向 Host 根。离线 Host 管理入口也必须先通过版本准入，不能在缺少 host.toml 的旧根直接创建它而跳过升级。

企业首次用户配置只初始化必要默认值，不复制个人 Provider、Speech、MCP、权限或 Skill。缺少可选配置与存在但损坏要区分：后者显示本人配置错误，不自动覆盖为默认文件。

目录键、配置来源、旧个人 Skill 根和模型来源在启动/用户域装配时解析。配置、存储、Skill、MCP、Speech 的普通读写只接收已绑定的源和根，不在每次业务操作中用模式重新选择路径；明确的托管模型编辑限制在模型管理边界处理。

### 1.3 版本字段为何存在

`version` 是用户明确要求的 Host 配置/目录升级版本，消费者为启动前升级分派器：缺失文件进入旧布局升级；低版本按顺序升级；相同受支持布局直接打开；高版本、非法版本、缺失字段或解析失败拒绝启动。它不表示每次启动的应用版本，不参与并发 CAS，也不随用户设置变更递增。

本字段的依据是本轮明确指令及已经发布的旧目录兼容需求，属于存储格式准入。配置并发仍使用现有内容摘要 revision，数据库仍使用现有 `schema_migrations` 和最低兼容 Host；不再增加 layout_revision、迁移步骤表或用户目录版本表。验证覆盖版本不匹配、最终提交前后中断、重复启动和未来版本拒绝，不能用版本相等代替用户库完整性核验。

### 1.4 发包时内置默认中心地址

用户明确要求企业发行包默认带入 `center_url`，通过发包环境变量提供。拟使用构建变量 `EZ_ASSISTANT_DEFAULT_CENTER_URL`，例如 `https://center.example.com`；值编入随包 Host，安装后的机器不需要再设置该环境变量。Desktop、Client 和 Host Web 均以 Host 的有效配置为准，不各自维护一份前端默认地址。

默认值只用于创建企业配置：本机管理已明确指定的 `center_url` 优先，否则采用随包默认值，并写入 `host.toml` 的 `[enterprise].center_url`。后续启动读取该文件；升级或安装另一个默认地址的包不能覆盖已有中心配置，也不能清除 `center_id`。个人模式升级仍保持个人模式，内置地址本身不触发模式切换。已有企业配置缺字段或损坏时按配置错误处理，不能用新包默认值静默改连其他中心。

构建接入复用 [Host build.rs](../../../../../apps/runtime-host/build.rs)：读取变量、按中心 URL 规则校验并生成内置默认值，声明 `cargo:rerun-if-env-changed=EZ_ASSISTANT_DEFAULT_CENTER_URL`，避免换地址后继续复用旧编译结果。地址只包含服务位置，不包含密码、Token、key 或预先绑定的 `center_id`；中心身份仍在首次成功认证后核验并保存。

发包链路必须把变量传到实际编译 Host 的步骤。[Desktop Host 构建脚本](../../../../../apps/desktop/scripts/build-host.mjs)已有环境传递；[Client npm 装包脚本](../../../../../apps/client/scripts/pack-npm.mjs)只复制预构建 Host，因此仅在 npm 装包时设置变量不会修改已构建二进制。Desktop 安装包和各平台 Client 包均须包含按目标默认地址构建的 Host。

C03 实施默认值读取及企业配置初始化；C06 接入正式发包入口并校验企业发行包的默认地址非空、合法且实际生效，个人发行包允许不设置。验证覆盖无运行时环境变量的新装、显式地址优先、升级保留已有地址与身份，以及更换构建变量后制品默认值随之更新。本轮只定义契约，未修改构建脚本或生成制品。

## 二、确定性的目录与文件归属

企业目录名固定为 `<小写标准 UUID>_<十进制正整数 user.id>`，例如 `users/01234567-89ab-4cde-8f01-23456789abcd_42`；只从已校验类型编码，禁止自由文本拼接。个人固定为 `_personal`。路径能反映两个稳定身份维度，不保存 Token、用户名或中心 URL。

| 旧根内容 | v0.27.0 目标/处理 |
| --- | --- |
| `config.toml` | 拆分 Host 节和个人节；保留注释及未涉及字段，旧原件在整套备份中保留 |
| `data/` | 整体迁到 `_personal/data/`，含 SQLite、Session JSONL、Workspace 私有区、blobs、staging；不是只复制数据库 |
| `skills/` | 整体迁到 `_personal/skills/`；Host 公共目录在最终切换后从空目录开始，旧私人内容不公开 |
| `mcp.json`、`mcp/` | 迁到 `_personal` 同名位置，包括服务器默认工作目录 |
| `permissions.json`、`recall-reference.key` | 迁到 `_personal` 同名位置；签名密钥保持原值，旧 Recall 不因重新生成密钥失效 |
| `device/` | 迁到 `_personal/device/`，保留原 Gateway 身份、证书与配对文件；其他用户在自己的域内初始化，不复用旧个人身份 |
| 既有 `backups/database/`、`backups/configuration/` | 分别迁到 `_personal/backups/database/`、`_personal/backups/configuration/`，不作为新布局的整套备份证据 |
| `run/` | 保持 Host 级；复用现有实例锁，发现信息在当前启动重新发布，不把旧 bootstrap 当用户凭据 |
| Host TLS 的显式证书/私钥路径 | 外部文件不搬迁；若明确引用本次已迁移托管文件，仅重定位该配置字段，文件内容不改 |
| 外部 Workspace、显式 MCP cwd、外部个人 Skill 根 | 保持原位置；仅原本指向被迁移托管目录的结构化路径随目标修正 |
| 未识别的根文件/目录 | 保留原位置且不向普通用户暴露；不能自动归为公共资源，诊断给本机管理者，不阻止已知数据迁移除非发生目标冲突 |

含业务内容的运行日志/缓存由用户域装配路径决定，Host 公共日志只记录脱敏进程诊断。Tauri 自身应用偏好/WebView 数据位于平台应用目录，不借本次 Home 升级跨目录搬迁；原浏览器 Profile 在原生连接装配时归原个人域，其他用户独立选择自己的 Profile，浏览器操作本身不按模式分支。实现时新增或发现其他托管资源需补齐本清单，不能默认保留全局。

## 三、启动分派与 v0.27.0 程序

### 3.1 启动入口

沿用 `prepare_runtime_home` 与 `RuntimeInstanceGuard`，锁覆盖整个 Host 根。锁取得后、监听开放前调用具体 `host_layout::upgrade`；`run/` 的临时发现不能让客户端误判业务已就绪。

```text
不存在 host.toml
  → v0.27.0 升级（空 Home 走同入口初始化）
  → 成功写 version = 0.27.0
已存在 host.toml
  → 解析 version
  → 执行缺少的布局升级，或直接使用，或拒绝不支持版本
完成布局准入
  → 装配 Host
  → 各用户数据库在打开时分别做既有版本准入
```

不存在 host.toml 是选择升级程序的依据，不等于授权覆盖任意目标。`users/_personal` 已有内容且不能证明为本程序上次结果时停止；缺少旧数据库但还有会话/文件时不得当作全新空库。真正全新 Home 可以不创建个人数据库，待个人 Runtime 打开时初始化。

### 3.2 旧数据升级顺序

程序在实例锁内自动执行以下步骤，不增加人工逐项操作页面：

1. 识别固定旧文件清单，读取原配置与实际数据库路径，验证受支持布局和目标无未知冲突。数据库只读准入识别 Schema/兼容及 journal 状态，不用不存在的文件路径打开新库。
2. 在 `backups/host-layout/v0.27.0-<随机后缀>/` 创建独立整套备份。SQLite 复用现有 Online Backup 与表数量/内容/Schema 校验；配套目录单独复制并核对相对路径、文件大小、摘要及符号链接目标，不跟随链接搬走外部工作区。备份 manifest 原子发布前不改源数据。
3. 将第二节列出的个人目录/文件逐项原子 rename 到 `_personal`；同一 Host 根下不默默退化成跨卷复制后删除。配置以原件为依据，用 toml_edit 写入去除 `[host_access]` 的个人文件及待提交的 Host 文档；每个目标只创建或验证，不覆盖未知内容。
4. 在目标个人库执行既有数据库版本链及本版的路径重定位事务；数据库迁移仍归 storage/migrations，Host 布局模块只传入明确的旧/新受托管根。重定位与 `0.27.0` 数据库账本/最低兼容 Host 更新原子提交，不能移动一个文件就写一次数据库版本。
5. 核对最终配置、数据库、文件和路径，确保根 `skills/` 的旧内容全部已归个人；确认旧根 config 与备份原件一致后移除旧根副本，清理仅本程序创建的暂存文件。用户业务内容没有双写来源。
6. 最后以临时文件落盘并原子 rename 发布 `host.toml`，其中 `version = "0.27.0"`。到这一步才允许装配业务服务和创建空的公共 Skill 根。成功后的再次启动只读版本，不重新搬迁旧布局。

备份对象包括旧配置/权限/密钥、MCP/Skill/device、整个 data 及既有个人数据库/配置备份；排除当前运行的 run 和正在创建的整套备份本身。未识别根项不移动、不修改，恢复时也保持原位。这个 manifest 是备份内容及恢复来源证明，不维护可变的步骤状态或另一个“当前布局版本”。

旧数据库若需要更早的已支持升级，仍按现有数据库链执行。无旧库的纯配置 Home 只迁配置和文件，不伪造数据库迁移记录。各企业域以新布局创建；之后单独打开已有用户库时执行其数据库准入，不再次执行 Host 目录升级。

### 3.3 中断后重试

没有最终 host.toml，下一启动仍调用同一 v0.27.0 程序。用原子完成的备份 manifest 与源/目标现状判断，不新增通用迁移服务：

| 当前状态 | 行为 |
| --- | --- |
| 仅未完成备份，没有原子发布的完整 manifest | 源尚未写入；保留未完成备份，重新备份后才迁移，不把它视为可恢复副本 |
| 源存在、目标不存在 | 核对源与已验证备份一致后执行该项 rename |
| 源不存在、目标存在 | 核对目标为备份的迁移结果，跳过已完成项；已提交数据库按对应迁移账本与目标投影验证，不要求它字节等于旧库 |
| 源、目标都存在，或均丢失但备份证明应存在 | 停止，不合并、不选择“看起来较新”的一份，不在重试中自动恢复覆盖 |
| 个人 config 已拆分，根 config 尚在 | 按备份原件计算唯一预期输出，目标相同则继续收尾；不同则冲突，不覆盖后来编辑 |
| 数据库事务尚未提交 | 由 SQLite 恢复及现有安全准入判定，再执行未完成版本；未知/不安全 journal 按现有规则报错 |
| 数据库已提交、host.toml 尚未发布 | 验证 DB 账本、重定位投影和其他目标，完成剩余收尾及最终版本提交 |
| 有多个无法唯一确定来源的完整恢复 manifest | 报错并保留全部文件，不猜测恢复来源 |

检查失败立即停止后续写入；不开放半成品、不回退旧根建空库、不自动连续补救。错误对本机管理者给出阶段和备份位置，对未认证客户端不暴露用户路径。

配置和权限等明确转换的文件，以备份原件及固定转换函数计算预期结果后核对；原样搬迁文件才按原摘要比较。根 config 已移除而个人 config 完整、输出符合预期也是已完成收尾，不能套用原样 rename 项的“双端丢失”判断。

## 四、路径与历史数据

### 4.1 结构化当前定位

按路径组件匹配“旧根下确实被本次迁移的目录”，转换为 `_personal` 中目标；外部路径保持原样，已经属于目标根的路径不得二次套入 `users/_personal`。

| 实际字段/文件 | 处理 |
| --- | --- |
| `workspaces.user_directory / additional_directories_json / agent_directory` | 只转换被搬迁的托管路径；用户选的外部根不变 |
| `session_resources.working_directory / additional_workspace_directories_json / attachment_directory / private_directory` | 同一事务精确转换；恢复、文件权限及默认 cwd 使用新位置 |
| `attachments.agent_readable_path` | 更新托管附件的当前物理地址；Blob 的相对键保持原相对语义 |
| `inputs.queued_message_json` 中尚未写入 Conversation 的 `FileReferences.readable_path` | 按已核对附件映射更新结构化定位；正文、ID、Skill 上下文及其他队列事实不改 |
| MCP JSON 的显式 cwd、权限文档中的结构化路径选择器、Host TLS 路径 | 只解析已定义的路径字段并转换，目标文件使用原子替换；不能改普通环境变量、脚本正文或自由文本 |
| Conversation JSONL、消息正文、工具参数/结果、冻结 Skill、历史 SystemPrompt、C02 派生快照及摘要 | 保持原内容；不做全文替换，不重算历史证据冒充当时就使用新目录 |

数据库升级校验逐表精确数量；不应变化的表/字段比较摘要，应变化的定位字段按转换映射核对。文件校验区分原样文件与明确转换的配置；存在文件和数据库计数相同都不能单独证明附件引用可读。

### 4.2 旧历史中的绝对路径

历史显示保留当时路径。受控资源引用仍先通过本域 Store 解析 Session/child/附件归属，再解析物理文件；仅 `_personal` 的历史资源 resolver 兼容旧 `<host_root>/data/sessions/…`、`data/workspaces/…`、`data/blobs/…` 到当前个人根的映射。要求引用来自已保存的结构化资源记录、所属资源在本人库存在且映射后符合相同资源约束，不能把客户端任意绝对路径当作旧引用。

已提交消息的 `FileReferences.readable_path` 同样使用这个 resolver：预览及 Fork 的附件映射使用当前路径；再次准备模型输入时只在请求副本中转换已验证的结构化 FileReferences，不修改原 JSONL 或恢复校验所依赖的快照。历史正文中的自由文本链接仍按历史显示，不能全文替换成新的证据。原始/派生快照的 C02 一致性校验先完成，当前请求目录适配与 disclosure 随后进入正常输入编译和预算流程。

映射不适用于企业用户、不适用于任意 `/host-files` 请求，不给根 `skills/` 建别名，也不创建指向 `_personal` 的全局 symlink。新布局不再在旧 `data/` 写业务内容，因此这几个保留前缀没有新旧来源歧义。

旧路径映射只在已迁移个人 Store/资源 resolver 的装配中提供，正常业务统一调用该 resolver；不能为每个下载、Fork 或模型输入入口复制 `if personal` 兼容判断。

旧 Session 的冻结 SystemPrompt 中仍可能有旧 `<runtime_directories>`。后续执行按实际 SessionEnvironment 生成最新目录说明，复用现有 Run disclosure/InternalContext 注入路径，在模型输入中明确以当前目录为准，并计入上下文预算；child 继承同一规则。它不修改持久 Conversation/C02 摘要，也不绕过压缩与 token 预算校验。新 Session 的原始提示直接使用新路径。

普通正文、任意脚本、Shell 参数中的旧绝对路径不自动重写；用户显式执行硬编码旧路径可能需要改为当前路径。产品不承诺对任意历史脚本提供虚拟文件系统，也不自动重放旧工具。此限制不影响通过产品资源 ID 访问已迁移附件和会话文件，应在升级说明中表达。

## 五、备份、恢复与验证边界

整套备份必须实际可读：SQLite 逐表核对、Schema/内容摘要及完整性检查，文件逐项摘要核对，密钥与配置受私有权限保护。已有 `storage/migrations/backup.rs` 的 `external_files_included: false` 不能改个字段就当作全文件备份；布局升级必须真正完成对应文件复制和校验。

恢复是显式离线操作：停止 Host，保留失败现场，核对独立备份后恢复旧根布局和匹配的程序版本；不能把旧数据库单独覆盖到新布局继续运行。多个文件 rename 与 host.toml 提交不能组成一个数据库事务，整套备份是这些步骤失败后的恢复来源；程序失败默认保留现场，不自动回滚覆盖。

v0.26.0 二进制不认识 host.toml，不能被这个文件可靠阻止误写旧根。因此不支持直接替换旧程序降级；C06 必须给出整套恢复说明和制品路径验证。目录版本不替代数据库最低兼容 Host，新目标库声明 `0.27.0` 最低兼容；已归档历史版本文件不回写。

后续实现验证使用临时 Home/隔离库：空安装、标准 v0.26.0、纯配置、自定义 Home、外部 Workspace、私有 Skill、Recall、MCP、附件/child 资源、C02 冻结上下文；逐个写边界注入中断，覆盖磁盘不足、源目标冲突、损坏库、非法/未来版本与完整恢复。Windows 文件占用、原子替换及平台安装升级由 C06 补充真实验证。

程序内备份核验与开发时数据库操作分别遵守其边界。设计阶段没有数据库操作；M0 实现仅使用临时隔离 SQLite 夹具，后续人工迁移/恢复真实数据时，仍按根 AGENTS.md 核对实际环境、适用授权、备份和影响结果。设计授权不自动代替生产数据操作确认。
