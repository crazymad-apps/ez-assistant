# Enterprise Center

企业中心后端，版本 `0.1.0`，Node 24、NestJS/Fastify、PostgreSQL 16.14、TypeORM 1.1.1（pg 驱动）、node-pg-migrate 9.0.0。
提供数据库升级、超级管理员初始化、协议信息、统一身份、用户管理、管理重置、审计查询及模型管理；正式 Admin 独立构建后同源合并交付，C04 M1 已增加模型代理与调用追溯。C01 M0—M3 已完成并获确认，C01 循环验收状态见[开发计划](../../docs/versions/v0.27.0/cycles/C01-企业身份与中心管理基础/开发计划.md)。

## 构建与验证

```bash
npm ci
npm run build
npm run typecheck
npm run lint
npm test
```

测试使用真实 tsc 装饰器编译产物。源码在 src/，测试在 tests/；构建会清理本包 dist，不手改产物。

## 同源制品装配

开发时两包分别安装依赖。以下命令分别构建 Admin 和 Center，再装配一个全新的临时制品目录：

```bash
npm run build:bundle
```

命令末尾输出 JSON 的 `directory` 为制品路径；`npm run assemble` 只执行装配，要求两包此前已分别构建。
装配不覆盖旧目录、不复制 .env、源码、原型或开发依赖，也不执行数据库操作；正式分发流水线归 C06。
制品包含后端 dist、package.json/lock、README/.env.example 以及 public/admin 与 public/site 静态页面。

在输出目录中：

```bash
npm ci --omit=dev --ignore-scripts
# 填写并核对该部署的 .env；已有库升级须先按项目规则备份、恢复核验和审批。
node --env-file=.env dist/main.js
```

非 Docker 完整模式只有一个 Node 服务，`/`、`/admin/` 与 `/api/` 同源；不需要 Vite、TypeScript、前端 node_modules 或额外静态服务器。
默认入口先依据 Vite manifest 校验首页、共享/延迟加载资源，缺失、空文件或清单无效时返回 ADMIN_ASSETS_INVALID／SITE_ASSETS_INVALID 并在连接数据库前退出；这不是文件内容签名验证。
后台设置同源脚本 CSP、nosniff、no-referrer、禁止嵌入及 no-store；Ant Design 动态样式保留 inline style。
构建清单、隐藏文件和非后台目录不提供静态访问；未知资源和 API 返回 404，不回落首页。

仅后端开发使用 `npm run start:api` 或 `node --env-file=.env dist/main.js --api-only`；它明确跳过公开页与后台装配，不作为完整产品交付。

### 实时开发预览

后端在本工程运行 `npm run dev`，前端在 `../enterprise-admin` 运行 `npm run dev`，浏览器访问 `http://127.0.0.1:7310/admin/`。
前端 Vite 热更新并将 `/api` 代理到 `127.0.0.1:7320`。后端 `.env` 设置 `CENTER_PORT=7320`、`CENTER_PUBLIC_ORIGIN=http://127.0.0.1:7310`；连接已经核实的开发库时建议 `CENTER_DATABASE_AUTO_UPGRADE=false`，只检查升级账本。
后端 dev 使用 Node 24 原生 `--watch-path`（macOS/Windows），监听 src、tsconfig.json、package.json 与 .env；每次先完整编译和复制 SQL，再启动 API-only 服务，编译失败不运行旧代码。自动加载 `.env`，环境变量优先。Linux 不支持此 watch-path 入口，可使用显式构建加 start:api，后续按实际开发平台需求调整监听方案。
后端源码或配置变更会重启服务，内存 Token 随之失效，需重新登录；仅前端页面刷新会从 Cookie 恢复有效身份。dev 不是生产部署方式。

## 数据访问与升级边界

- 身份、用户、审计与模型配置通过各模块 TypeORM Entity 映射，普通 CRUD 使用 Repository/QueryBuilder；用户/审计查询归属各模块 repository，复杂分页保留单语句参数化 SQL。API DTO 不继承 Entity，密码摘要默认不查询，身份核验显式读取。
- `synchronize=false`、`migrationsRun=false`、`dropSchema=false`；表结构、初始化数据和升级账本只由 node-pg-migrate 的 SQL 管理。不新增配置、账号或 ORM 升级命令。
- 启动先完成升级/准入并关闭维护 Pool，再初始化业务 DataSource 和 HTTP；关闭顺序相反。使用现有显式 Nest Provider 装配，不再建立另一份连接生命周期。
- 业务与审计共用 QueryRunner 的事务 Manager。COMMIT 成功后才变更内存凭据；提交结果不明清空凭据、不重试。回滚也失败时关闭业务池并拒绝业务请求，须排查后重启。

## Swagger / OpenAPI 与前端交接

- 服务运行时：`/api/docs` 为 Swagger UI，`/api/openapi.json` 为 OpenAPI JSON。
- 离线导出：`npm run openapi:export`，生成本包根目录 [openapi.json](openapi.json)。不读取数据库连接配置、不连接数据库、不监听端口。
- 一致性检查：`npm run openapi:check`，快照过期时失败；接口变更需重新导出，不直接修改生成文件。
- 前端从该 JSON 文件或在线地址生成类型与请求方法，生成工具及 `src/request/openapi/` 由前端工程维护。前端日常构建使用生成产物，不依赖在线后端或后端源码。

采用 `@nestjs/swagger` 12.0.1 和 Fastify 静态资源依赖 `@fastify/static` 10.1.4，按 [Nest 官方集成方式](https://docs.nestjs.com/openapi/introduction) 从 Controller/DTO 生成 OpenAPI，不另维护手写规范。稳定 operationId 用于生成方法名，当前包含 getCenterInfo、login、getCurrentIdentity、logout、changeOwnPassword；M2 新增 listUsers、createUser、updateUser、resetUserPassword、listManagementAudit。

Swagger 文档当前随服务可访问，不提供额外鉴权；生产可在反向代理限制 `/api/docs` 及其资源和 `/api/openapi.json` 的访问。页面资源本地提供、外部 validator 和凭据持久化关闭；文档不得包含真实凭据，Swagger 注解不替代接口自身的认证、授权或输入校验。

## M1 身份接口

Center 允许任意来源跨域访问，不按 `Origin` 或 `Sec-Fetch-Site` 拒绝请求；CORS 预检无需登录，
支持各业务方法和请求头，管理响应与模型代理流均返回跨域响应头。实际受保护接口仍要求显式 Bearer，
权限校验照常执行；不使用 Cookie 认证，也不启用 CORS Cookie 凭据。`CENTER_PUBLIC_ORIGIN` 用于
下发模型代理和 Desktop 更新下载地址，必须可从 Host 与 Desktop 访问，不限制浏览器页面的访问地址。

| 接口 | 输入与返回 |
| --- | --- |
| POST /api/auth/login | `{username,password}`；200 返回 center_id、user、token 和 llm_key |
| GET /api/auth/me | 有效身份凭据；200 返回中心与本人身份，不再次返回秘密 |
| POST /api/auth/logout | JSON `{}`；204，仅删除当前 Token 和关联 key，重复或未知凭据幂等成功 |
| POST /api/auth/password | `{old_password,new_password}`；204，验证原密码，更新本人密码，保留已有 Token/key；管理重置任意用户使用下节独立接口 |

账号 trim 后转小写，密码不裁剪；新密码 6—128 个 Unicode 字符，必须包含英文字母和数字，最多 512 UTF-8 字节，不要求大小写并存或特殊符号。历史密码与初始化密码仍可用于登录/原密码验证。拒绝审计只记录固定原因码，不记录账号输入、密码、Token/key 或摘要。HTTP 解析失败、过大请求和底层故障只返回脱敏错误，不承诺故障时审计一定能落库。

密码采用异步 scrypt，最多两个并行计算和八个等待者；登录每真实 TCP 来源每分钟 20 次，最多保留 1024 个来源，超额返回 429。当前 trustProxy=false，不信任转发头；反向代理后的请求按代理地址共享限额，真实客户端代理信任策略需部署时评估，不自行信任 X-Forwarded-For。

可供前端/C03 使用的脱敏身份契约夹具位于 tests/fixtures/identity/，标识和秘密替换为保留格式的固定合成值；正式接口定义以 openapi.json 为准。

## M2 管理接口

全部要求管理员权限：is_super_admin=true 或 role=admin，且用户启用。没有固定用户名/ID 判定。数据库直接给其他用户赋予标记立即被识别；即使 role=user 也具有管理权限。此类人工 SQL 仍须按数据库安全流程操作，产品不自动补记运维审计。

| 接口 | 输入与返回 |
| --- | --- |
| GET /api/users | 可选 search/role/enabled/limit/offset；200 返回 items/total/limit/offset |
| POST /api/users | username/display_name/role/enabled/password；201 返回公开用户信息和创建/更新时间 |
| PATCH /api/users/{id} | display_name/role/enabled 至少一项；200 返回保存结果；不可改账号或超级管理员标记 |
| POST /api/users/{id}/reset-password | 仅 new_password；204；可重置自己、其他管理员或超级管理员，不收原密码 |
| GET /api/audit | 可选 id/from/to/actor_user_id/action/success/limit/offset；id 为审计数字 ID 精确筛选，200 返回只读审计页 |

列表 limit 默认 20、最大 100，offset 默认 0、最大 1000000。search 是字面子串；日期为 UTC ISO Z 格式，from 含、to 不含。用户按创建时间/ID 升序，审计按时间/ID 倒序；单次查询的列表与总数共享 SQL 快照。拒绝未知字段、重复参数、错误类型、非法分页和日期。

禁止通过 API 停用或降权超级管理员，不提供授予/清除标记或删除用户的接口。至少保留一个启用管理员。角色/启用状态实际变更后撤销目标全部 Token；本人改密和管理重置均保留已有 Token/key，编辑显示名称不撤销，相同值不重复写入/审计。新密码统一要求 6—128 字符并包含英文字母和数字（512 UTF-8 字节以内）。

常见错误：409 USERNAME_EXISTS / SUPER_ADMIN_PROTECTED / LAST_ADMIN_REQUIRED、404 USER_NOT_FOUND、403 ADMIN_REQUIRED。管理重置与个人改密是不同接口，不根据目标是否为自己改变校验。操作成功后自身 Token 失效时，前端应返回登录页。

前端从 OpenAPI 生成请求方法和 DTO，后台入口按超级管理员标记或管理员角色判断，不只看用户名或 role 文本。正式页面和同源打包已在 M3 完成。

## 登录与全局鉴权

登录只提交 username/password，返回 center_id、user、token 和 llm_key。原 delivery、session_id、session_token 已删除，不保留未发布字段兼容。普通用户和管理员共用接口，管理 API 另行校验管理员权限。

Token 直接保存在单实例进程内 Map，不做哈希、不入库、无 TTL；多次登录互不覆盖。退出只删除当前 Token 和关联 key；本人改密和管理重置保留已有 Token/key；进程关闭/重启全部失效，需要重新登录。用户密码仍为 scrypt 摘要，持久用户与审计不受重启影响。

全局 [security.ts](src/security.ts) 默认要求 Authorization: Bearer；/api/info 和登录公开，退出可无效/无 Token 幂等调用，Swagger 文档明确公开。Cookie 不参与后端鉴权，后端不设置/清除 Cookie，前端负责保存/清除凭据。鉴权失效返回 401 TOKEN_INVALID。默认支持 HTTP，HTTPS 可通过反向代理扩展；不将 Token/key 写日志或 URL；没有多实例共享登录能力。

LLM key 仅在内存关联本次 Token，不能调用身份 API。LLM 代理由 C04 M1 实现，见本文调用记录章节。登录成功审计提交后才加入 Map；退出/改密审计失败时返回失败且不撤销凭据；提交结果不明时清空内存凭据并失败。身份写入串行到提交后的内存变更结束。

本次直接调整未发布的 001-initial.sql，不新增兼容升级脚本；旧试验库保留，不原地删表，新安装/验证使用新库。

## 配置

标准模板见 [.env.example](.env.example)，只列出主要启动配置项。模板采用本机 HTTP 开发配置，数据库连接留空，必须明确填写；生产差异见文件注释。

在本包目录使用：

```bash
cp .env.example .env
# 编辑 .env，填写并核对数据库目标
npm run build
node --env-file=.env dist/main.js --api-only
```

除 `npm run dev` 外，程序和其他 npm 启动脚本不自动加载 `.env`；上例为仅 API 开发，使用 Node 24 原生加载，已有进程环境变量优先。合并制品在部署平台注入配置后使用 `npm start`；`.env` 已被 Git 忽略，不提交真实凭据。

使用文件配置执行数据库命令时，分别使用 `node --env-file=.env dist/database/cli.js check`（只读）和 `node --env-file=.env dist/database/cli.js upgrade`（写入，操作前仍按数据库安全规则确认/备份）。

| 环境变量 | 默认值与用途 |
| --- | --- |
| `CENTER_DATABASE_URL` | 必填 PostgreSQL URI；升级和业务共用一个账号，无默认数据库连接 |
| `CENTER_DATABASE_AUTO_UPGRADE` | `true`；只接受 `true` / `false` |
| `CENTER_HOST` / `CENTER_PORT` | `127.0.0.1:7320` |
| `CENTER_PUBLIC_ORIGIN` | 必填中心公开地址，无路径/尾斜线；用于下发模型代理与 Desktop 更新下载地址，须可从 Host 与 Desktop 访问，不是请求来源白名单 |

初始 SQL 内置超级管理员 `admin / 123456`，只保存预生成的带盐 scrypt 摘要，不另设初始化代码、命令或配置。该公开默认密码应在正式使用前修改；它是后续新密码长度规则的初始化例外。

## 启动与数据库升级

只需一个应用数据库账号，预先具有本应用专用库及 public schema 的建表、改表和业务读写权限；不需要 PostgreSQL 超级用户、建库/建角色权限或操作系统 root，也不切换账号、临时提权。不自动创建数据库或认领无版本记录的非空库。

- 自动升级开启：执行待完成 SQL（首个 SQL 包含初始数据）→ 检查数据库版本 → HTTP 监听。
- 自动升级关闭：只读检查数据库版本；空库或待升级库返回 UPGRADE_REQUIRED，退出且不监听。
- 手动执行：`npm run db:upgrade`，使用同一连接完成升级与首次初始化；不受自动升级开关限制。
- 只读检查：`npm run db:check`，输出脱敏目标、版本与四表精确数量，不建表、不初始化。
- 框架通过升级记录保证初始 SQL 只执行一次；重启或后续升级不重新创建管理员，不覆盖用户名或密码。

SQL 放在 `src/database/migrations/`，如 `001-initial.sql`、后续 `002-xxx.sql`。只维护文件，不维护第二份 manifest；已发布文件名及内容不改写。node-pg-migrate 负责排序、升级锁、事务和唯一版本表 `pgmigrations`；项目只补充未知/更高版本及未知非空库准入，不做逐字段结构比对。不支持生产 down/fake 或跳过锁的入口。

所有待执行业务 SQL 与对应版本记录同事务，失败全部回滚并退出。`001-initial.sql` 同时创建结构、中心身份、超级管理员和初始化审计，不另开初始化事务。框架首次建版本表发生在该事务之前，所以首次失败可能留下空 pgmigrations 及序列，不会留下已完成升级记录、业务表或初始用户。

超级管理员初始化属性：`role=admin,is_super_admin=true,enabled=true`。重复执行不会再次插入；并发执行由框架升级锁保护，锁冲突明确失败，不开放公网初始化接口。后续用户权限和禁止删除规则由 M1/M2 服务端业务实现，单账号本身具有数据库写权限，不宣称审计不可被直接 SQL 修改。

既有库升级前仍须停写、独立备份并实际恢复核验；自动升级开关不代替生产升级审批/备份。首次未发布实现的旧 schema_migrations 测试库不做兼容转换，保留旧库并另建隔离库。

PostgreSQL 实测基线 16.14，准入允许后续 16.x 补丁，不自动接受其他主版本。连接/查询/锁等待有界，SIGINT/SIGTERM 先停止 HTTP 再关闭业务 DataSource，最终退出上限 10 秒。

## 隔离集成测试

明确提供：

- `CENTER_TEST_DATABASE_URL`：`center_app` 应用账号，完整集成测试目标 `127.0.0.1:55432/ez_center_c01_test_m1_<run>`。
- `CENTER_TEST_ADMIN_URL`：同一隔离实例的 postgres 建库连接，仅测试夹具创建使用，不进入产品配置。
- `CENTER_TEST_RESTORE_DATABASE`：`ez_center_c01_restore_m1_<run>`，已实际恢复的独立备份库。
- `CENTER_TEST_BASELINE_BACKUP_VERIFIED=true`：操作者已完成 pg_dump/pg_restore 与逐表精确数量/关键字段核验的声明。

源库先用应用账号运行 db:upgrade，初始 users/center_identity/management_audit/pgmigrations 各 1 行，无 login_sessions 表。使用容器内 PG16 工具独立备份恢复；备份不含集群角色，恢复库需预先由同一应用账号拥有。随后运行 `npm run test:integration`。

测试重新核对源/恢复库全部已存字段（密码摘要不打印），从已验证恢复库复制升级夹具；主库故障场景事务回滚，成功升级只提交到新夹具。新建库、角色与本机 SQL 夹具保留，无 DROP 清理。首个意外失败即停，不连续补救。

M0—M3 已确认；C01 循环确认、提交和阶段归档仍为独立门禁，不代表整个 v0.27.0 或企业分发已完成。

## 模型管理与独立模板（C04 M0）

后台“模型管理”维护服务商连接、在线目录、固定参数和唯一企业默认模型。显式刷新完整成功才替换数据库中的在线快照（允许空列表）；失败保留原快照，连接变更后的迟到结果拒绝提交。固定配置独立存储，刷新目录或模板均不覆盖它；删除服务商保留失效默认引用，后台显示原因并要求重新选择。API Key 仅写入，查询返回是否已设置。

列表顶部“公共配置”将企业默认模型和客户端模型访问地址并列。地址仅包含 HTTP(S) 主机与端口，
例如 `http://localhost:7320`；管理员保存到 `model_settings.public_origin`，客户端下次读取模型配置即生效，
服务重启后保留。留空恢复 `CENTER_PUBLIC_ORIGIN`。此地址用于下发客户端模型入口，服务商上游连接仍在服务商管理中配置。
`PUT /api/model-settings` 每次独立提交 `default_model` 或 `public_origin`，后者允许 null 恢复部署值。
升级到此版本前需按数据库安全流程执行 `008-model-public-origin` 迁移；迁移只新增可空字段，不更新既有数据。

初始模板唯一源文件为 [model-templates.json](../../packages/assistant-protocol/resources/model-templates.json)。构建复制到 `dist/resources/model-templates.json`，个人 Runtime 编译时嵌入同一文件。中心可设置 `CENTER_MODEL_TEMPLATES_FILE` 指向独立部署文件：保持 JSON 数组结构，按 `(provider_type, model_id)` 唯一匹配；`parameters` 为标准参数的部分字段，`protocol_parameters` 可按实际协议覆盖，`document`/`checked_on` 记录来源与核查日期。文件没有可执行表达式或动态版本号。

运维先准备完整文件并原子替换，再由管理员在后台“重新加载模板”。中心校验字段、枚举、数值及组合，全部通过才发布；无效文件保留旧有效模板，初次加载失败显示不可用。限制 8 MiB、10,000 项。模板只补在线未知值，固定配置优先；模板不增加在线目录条目。`xhigh` 是新配置的统一标准档位名，不接受旧 `x_high` 拼写，上游值仍由映射单独指定。文件替换不要求重新构建或更新整个中心。

迁移 `002-models-and-calls.sql` 新增 `providers`、`model_fixed_configs`、`model_settings` 及后续调用追溯所需的 `llm_recording_settings`、`llm_calls`；旧库需先按数据库操作规则备份/恢复核验，再升级。M0 提供 `managed_models`，`GET /api/runtime/model-configuration` 提供受身份保护的当前配置。中心代理、配置测试与调用追溯见下方 M1 说明；正式 Host 消费仍归 M2，当前不能据此认为正式企业客户端模型调用已接通。

## 模型代理与调用记录（C04 M1）

中心声明 `llm_proxy`。模型调用使用 `cl_` key，通过 `POST /api/llm/providers/:id/v1/responses`
或 `.../chat/completions` 访问当前默认模型；管理 Token 不能替代 LLM key。准入核对当前身份、
Provider/model/协议，不自动重试或跟随跳转。中心自产错误有 `x-ez-center-control: 1`，上游同名头
被剥除；已建立索引的响应含 `x-ez-call-id`。HTTP 2xx 完整转发不代表模型业务或任务成功。

管理接口保留 16 KiB 限制；模型代理只接受 UTF-8 JSON，单次正文 32 MiB、在途原始正文合计
128 MiB。普通 HTTP 路由的请求和响应字节不改写；显式 WebSocket 桥接按下述规则转换。`CENTER_LLM_CONNECT_TIMEOUT_MS` 默认 10000，范围 1000—120000；
`CENTER_LLM_IDLE_TIMEOUT_MS` 默认 300000，范围 1000—3600000，同时约束上游空闲和下游背压等待。
代理正常收完请求正文不会中断响应；取消、超时和关闭均销毁在途网络。

后台“调用记录”提供管理员分页/筛选、详情及按需查看请求/响应快照。“记录策略”控制正文采集和
保留期；模式为关闭、仅异常与未确认、全部调用，默认关闭，正文 7 天、索引 90 天。模式在调用开始时冻结。
仅异常模式也从开始时有界暂存，只有确认正常才丢弃正文；关闭不删除已有文件。模型参数页只能测试已保存且未修改的
固定配置，使用短非流式请求、30 秒期限与 1 MiB 响应限制；不会修改企业默认，也不证明全部能力可用。

正文按请求消息卡片和响应事件分栏阅读；Responses 的 `instructions` 显示为“系统提示词”，
连续增量按相邻归属聚合，异常和原始来源可展开核对。阅读投影只存在当前页面，收起、切换或权限失效后释放。
操作说明见[查看模型调用正文与异常](../../docs/site/manual/call-evidence.md)。

`GET /api/llm-usage` 为管理员提供按用户、时间（小时/天/月）及“服务商 / 模型”联合键的用量聚合；
`GET /api/llm-usage/models` 查询历史联合键选项。中心从上游 Chat Completions 或 Responses 的
流式/非流式响应提取 usage，每次代理尝试独立结算；输入、输出、总量及可获得的缓存/推理明细写入
调用索引，不依赖正文采集开关。未收到 usage、解析失败、超限和旧调用分别标为不可用或部分可用，
聚合只报告已知值及覆盖数，不推算缺失数据。统计使用 Asia/Shanghai 时间桶，默认查询最近 7 天的
业务调用；索引保留期决定可查范围，延长保留期不能恢复已删除记录。后台可从统计页下钻到调用列表。

启用正文采集前设置 `CENTER_SNAPSHOT_ROOT`，预先创建独立、绝对、非公开的持久目录。目录权限
0700、文件 0600；禁止符号链接。文件按 UTC 开始日期及调用 UUID 存放，各侧上限 8 MiB，采集与待写
缓冲共 64 MiB（小片段以固定页预留，含待写）；超限保留实际字节前缀并标记部分，不能裁掉 UTF-8 尾字节。
压缩／非 UTF-8 响应也保存实际字节，管理读取返回互斥的 text 或 body_base64，不自动解压。
正文读取最多两个并发槽，超出返回 SNAPSHOT_READ_BUSY；读取、Base64 及编码临时内存独立于采集预算。
WS 桥接正文标明来源，转换前首个失败帧单独保存为 upstream-failure.bin，上限 1 MiB，共享采集预算；
它不是已交付的响应，握手失败没有业务帧时不会伪造正文。未收到、合法空正文、采集失败分别表达。
文件先独占写入 `.part` 并 flush，使用同目录 hard link 原子发布固定名称后移除 `.part`，避免 POSIX
rename 覆盖已存在终态文件；快照根所在文件系统须支持同目录 hard link。

响应观察与 HTTP 转发、usage、快照完整性独立。观察支持 Responses/Chat SSE 和 JSON、compact 及计量；
分帧缓冲为单事件 1 MiB、JSON 8 MiB、进程共 16 MiB，超限不终止转发。只有可靠正常才按异常策略丢弃。
Center 不复制 Adapter 的完整校验，normal 不等于 Runtime 任务成功；Runtime 独有失败需用全部调用模式定向排查。
迁移 011 将旧 enabled=false/true 一次映射到 off/all，保持保留期；历史观察为空。旧后台写入缺 mode 时拒绝并提示刷新。
Center/Admin 配套更新，不改变 Host 企业协议版本。迁移不可原地降级；已有环境须按安全规则备份并恢复验证，
回退同时恢复匹配的程序、数据库及私有快照目录，不能直接启动旧 Center。

关闭先取消模型调用及目录刷新，等待调用索引和文件收尾最多 10 秒，再释放 HTTP/数据库。
重启将未结算调用标为 unknown，未提交完整性的已发布文件仅恢复为 partial；完整文件缺失/摘要
不符会单独标记，不改写可靠的转发结果。恢复异常停止启动；每小时最多清理 100 条，先删文件再改
状态/删索引。清理出错停止调度，排查后重启；未知文件和符号链接不会被递归删除。

部署与备份需同时包含 PostgreSQL、独立模板、快照根及插件包根；只恢复其中一项不能保证正文或插件包完整性。共享开发库
仍保持自动升级关闭。C04 M1 集成验证使用明确核对的新隔离库，按根规则完成备份恢复后定向运行
`tests/integration/calls.test.ts`，不直接运行会连接其他循环库的整组集成测试。

C01 M0 的 `POST /api/plugin-packages` 仅允许管理员上传 ZIP/TAR.GZ 完整包；首包建立未上架插件，
同名同版本同 SHA 返回 unchanged，异 SHA 校验通过后替换当前包引用。`GET
/api/plugin-packages/:name/versions/:version` 可在回包丢失后核对当前 SHA。上传暂存、内容寻址包与数据库
记录分属文件系统和 PostgreSQL，备份与恢复须覆盖两者。`CENTER_PLUGIN_ROOT` 默认为当前工作目录的
`data/plugins`，正式部署必须指向独立、绝对、非公开且仅中心进程可写的持久目录；中心不提供静态包路由。
启动会检查根、`staging`、`packages`，清理仅属于中心的遗留 `.part`，不自动删除无引用完整包。

C01 M1 在同一身份体系下开放管理员 `GET /api/plugins`、`GET/PUT /api/plugins/:name` 和
`PUT /api/plugins/:name/publication`。详情返回 ETag；说明和发布状态写入必须携带当前 `If-Match`，
过期条件返回 412。首次上架必须已有可用完整包，下架后状态为 deprecated，可重新上架。
普通已登录用户通过 `GET /api/runtime/plugins` 发现仅在架插件，通过详情查询已废弃/未上架状态，
再用 `POST /api/runtime/plugins/:name/resolve` 按平台、Host 版本和所需 Skill/MCP 名称选包。
`GET /api/runtime/plugins/:name/versions/:version/package` 必须带 Bearer 和当前 SHA 的
`If-Match`；同版本当前包已替换时返回 412 `PACKAGE_CHANGED`，下架后返回 410，且不提供
公开下载地址或 Range。下载以已打开的旧 SHA 文件句柄完成；下架提交后取消未完成的同名下载。
中心现声明 `plugin_market`，但不代表客户端安装、自动更新或 Host 侧 Agent 接入已经完成。

C01 M2 复用迁移 `003-agents-and-plugins.sql` 的 `custom_agents` 表，不新增 Agent 专用发布流程。
管理员通过 `GET/POST /api/agents` 和 `GET/PUT/DELETE /api/agents/:name` 读取摘要、创建、读取完整配置、
修改与删除；修改和删除必须携带详情返回的 `ETag` 作为 `If-Match`，缺失返回 428，旧条件返回 412。
创建后名称不可原位修改。已登录企业用户通过 `GET /api/runtime/agents` 读取完整配置数组；空数组是有效结果，
未授权与读取失败不解释为空目录。中心已声明 `managed_agents`，但不代表 Host 已安装依赖或能执行 Agent。

Agent 名称支持 NFC 中文，限制 1—64 个 Unicode 字符且不能有首尾空白或控制字符；描述最多 4096 字符，
完整提示词非全空、最多 256 KiB UTF-8 字节，保存时不裁剪换行或空格。Skill/MCP 引用各最多 256 项，
按同类名称去重；可不引用任何能力。引用带 `plugin` 时，新增引用必须被该插件至少一个当前包声明；
不带 `plugin` 表示由 Host 自行核对，不推断为中心插件。插件后来下架、替换或丢失能力不会删除已存引用，
管理员详情与列表会按当前包重新给出状态及支持平台，不能把中心诊断当作目标 Host 就绪结论。
管理列表不返回完整提示词，审计也不保存提示词或凭据。Agent JSON 写入上限 2 MiB；其余普通管理请求维持
原有上限。正式 Admin 的 Agent 编辑与依赖选择页面属于 C01 M3，目前尚未接入。

## Docker 交付

运行 `npm run package:docker` 在 Docker 外准备完整制品，再按 [部署说明](deploy/README.md) 上传和构建。Center 与 Nginx 入口镜像来自同一装配产物，镜像不安装依赖或编译；入口独占对外 7320 端口，Center 仅走容器网络；默认支持 HTTP，HTTPS 可通过代理扩展。

## F06 企业历史续接

v0.29.0 此能力使用企业协议 2；v0.30.0 F01 当前协议为 3（最低 3），Center/Admin/Host 须配套升级，旧端混用拒绝。迁移 `005-server-history.sql` 给旧连接补默认关闭及空能力覆盖，保留原配置、凭据、固定参数和调用记录。已有库升级前仍需停写、独立备份并恢复核验；不提供原地降级，回退须恢复匹配的数据库和文件备份。

管理员在现有服务商表单设置续接能力及开关；只有 Responses 且实际上游明确支持时可启用。旧缓存 Admin 缺少新策略字段会拒绝保存，刷新配套页面后重试。普通企业用户只读。

Responses 代理请求必须显式传入 `store`，无状态/配置测试使用 `store=false`；保存或旧 ID 续接使用 `store=true`，并携带配置下发的 `x-ez-center-history-binding`。该头仅由中心发送前核验，不转发上游。策略/主体/执行配置变更使旧绑定失效，Host 刷新配置并提示重试，不自动重放。绑定不依赖登录 Token，普通重登录及重启可恢复有效续接。


### Responses WebSocket 接续

管理员在服务商编辑页选择“Responses 上游传输 → WebSocket”，确认入口支持并启用服务端历史续接。
Host 仍走 HTTP/SSE；Center 建立独立上游连接，用不透明响应 ID 接续增量。业务流式 `store:true` 请求桥接并保留续接；v0.30.0 F01 同时支持普通 `store:false` 流式请求，响应结束后释放连接。非流式辅助生成沿用 HTTP。上游可保持 `store:false`，这不表示永久存储。
调用快照是 Host ↔ Center 的 HTTP 正文，不是协议转换后的上游原始字节。

| 启动变量 | 默认值 | 允许范围 |
| --- | --- | --- |
| `CENTER_LLM_WS_IDLE_MS` | 600000（10 分钟） | 1000—86400000 毫秒 |
| `CENTER_LLM_WS_MAX_CONNECTIONS` | 256 | 1—4096 |
| `CENTER_LLM_WS_MAX_PER_USER` | 32 | 1—全局连接上限 |
| `CENTER_LLM_WS_MAX_BUFFER_BYTES` | 16777216（16 MiB） | 1024—67108864 字节 |

缓冲上限同时约束单帧和每条活跃连接的待交付数据；慢下游受到既有 LLM 空闲超时约束。
连接达到上限时优先释放空闲链，全部在途则拒绝新建。空闲期限从完整交付后开始计时；最多 20 条空闲链不因超时回收，超过后按最后活跃时间回收已超时链至 20 条。全局及每用户硬上限仍优先约束资源，上游断线及身份/配置失效仍立即清链。
配置执行属性变化、关闭续接、用户失效时主动关闭相关连接；同值保存及同主体
更换登录 Token（含退出后重新登录）不主动回收；退出的旧凭据仍立即失效。同一用户的不同会话/子任务各自建链，压缩后全量重建不会关闭其他链。
配置回滚不发布失效通知；主体事务结果不明时沿用既有全部凭据撤销，同时释放桥接资源。

Center 重启丢失内存连接，旧 ID 由 Host 自动完整重建一次；普通网络或流中错误不重放生成。
修改这些启动变量需要重启 Center。部署为单实例，不提供跨实例连接共享或永久 response 查询。

## 反馈中心（v0.29.0 F04 M0）

迁移 `006-feedback.sql` 新增反馈单表，维护计数清单同步包含 `feedback`；已有用户库升级仍遵循独立备份、恢复核验和确认规则。
企业协议保持 2，可选能力新增 `feedback`。普通用户通过 `POST /api/feedback` 提交，`GET /api/feedback` 和 `GET /api/feedback/:id` 只读本人记录；
管理员通过 `GET /api/admin/feedback`、`GET /api/admin/feedback/:id` 和 `PATCH /api/admin/feedback/:id` 查询处理，正式后台入口为“反馈中心”。

每条反馈保存平台、插件或知识库目标及具体内容位置。同一用户与提交 UUID 重试返回原记录，内容改变返回冲突；
管理处理须提交原状态和原说明，避免覆盖他人修改。状态为已解决/已关闭时说明必填，提交者可见。
接口字段及限制见生成的 OpenAPI；列表最多 20 条，不含正文。当前 M0 只交付中心与管理端，用户客户端和 `submit_feedback` 工具待后续接入。

## F01 企业安装包管理

正式后台“安装包”支持上传平台文件、编辑版本说明、按平台发布和撤回。独立迁移
`007-desktop-releases.sql` 新增版本及平台目标两表，保留既有身份和其他业务数据。
已发布过的制品不能覆盖，撤回后仍保留原文件，重新发布使用相同文件。

发布机配置 `TAURI_SIGNING_PRIVATE_KEY` 或 `TAURI_SIGNING_PRIVATE_KEY_PATH` 及相应密码后，
Desktop 的平台发布脚本在平台签名/公证完成后生成 `release.json`、`release.json.sig`
及安装/更新文件。Windows 使用 MSI；macOS 使用 DMG 与最终 App 的 tar.gz。
清单必须由实际随包 Host 的 `--build-info-json` 生成，异构 Windows 目标需要在对应运行环境完成该步骤。
管理员选择同一输出目录中的清单、清单签名及 1—2 个制品即可上传，额外的制品 `.sig` 无需另选。

正式公钥由发布准备提供到 `packages/assistant-protocol/resources/desktop-update.pub`，构建时复制到
`dist/resources`；部署也可通过 `CENTER_DESKTOP_UPDATE_PUBLIC_KEY_FILE` 指定同一可信公钥的绝对路径。
当前仓库未内置正式密钥，没有公钥时 Center 其他功能可运行，安装包上传返回明确错误。
Center 从不接收或持有发布私钥；测试密钥不得用于正式发布。

`CENTER_DESKTOP_RELEASE_ROOT` 默认是工作目录的 `data/desktop-releases`，必须位于公开静态目录之外、
权限为仅服务账号可访问，且路径不能含符号链接。备份/恢复须将该目录与数据库配套处理。
中断上传只清理本次临时文件；数据库失败可能留下无引用的内容文件，不自动清理历史文件。

上传先验证小清单签名，再随文件流计算一次大小与 SHA-256，不解包、不运行安装器。
multipart 的 `manifest` 字段传原始 UTF-8 字节的 base64，避免浏览器改变换行使签名失效；
签名文本第二个字段，制品为后续重复 `files` 字段。清单最大 64 KiB、签名最大 8 KiB，
每个制品最大 512 MiB，总制品最大 1 GiB，两个并发上传、八个并发下载；上传总期限 30 分钟、空闲 60 秒。
发布与发现只读取小元数据和文件属性，不重复扫描大文件。

匿名 `GET /api/desktop-updates/:target/:current_version` 只返回已发布、适用且文件可用的较新版本，
无候选返回 204；固定版本及下载路径由该结果提供。草稿/撤回不可公开读取，撤回同时取消未完成下载。
支持 HTTP 和 HTTPS；HTTPS 由反向代理终止 TLS，`CENTER_PUBLIC_ORIGIN` 填实际访问地址，代理需允许上述上传大小及期限。
当前完成 Center/Admin 分发，Desktop 自动升级及真实平台安装验收在后续里程碑实施。


### v0.30.0 F01 上下文压缩

固定参数分别声明服务端压缩开关、服务端自行触发开关、compact 能力与输入计量能力。服务端自行触发本版不可开启；启用原生压缩要求 Responses 和明确 supported，自动与手动由 Agent 统一调用 HTTP compact。未启用时使用本地摘要，失败不跨机制回退。

中心固定代理 `/responses/compact` 和 `/responses/input_tokens`。HTTP 上游直接转发；WS 上游的 compact 使用独立连接发送有效输入及 `compaction_trigger`，完整终态校验成功后才返回整个规范窗口，不影响其他续接连接。计量始终使用 HTTP；不支持时保留既有 usage 与增量估算。配置绑定同时保护原生压缩，关闭响应 ID 续接仍保留配置周期。

迁移 `010-context-compaction.sql` 扩展既有 Provider 周期、固定参数与调用类型，不另建费用账本。消费聚合包括普通生成及压缩，计量请求不计消费。当前企业协议及最低协议均为 3；正式升级仍须按数据库安全规则备份、验证及确认。已核实的真实上游为订阅 WS V2；其他 HTTP compact 和计量组合须分别验证后声明 supported。

### 官网内容维护（v0.31.0 F01 M0）

按最新要求，更新日志直接在版本详情编辑，复用现有更新说明；首页与手册采用独立固定静态目录。
官网自动取各平台最新可用安装包，不另外选择官网版本或上传联合内容 ZIP。

维护约定见[官网内容维护](../../docs/site/README.md)，目标配置与接口见
[F01 技术方案](../../docs/versions/v0.31.0/features/F01-企业中心官网与公开页面/技术方案.md)。
M0 已实现并完成隔离验证：`site:build` 输出公开静态目录，`site:publish` 校验后更新，
`CENTER_SITE_CONTENT_ROOT` 配置固定内容目录；日常更新不连接数据库、不重建平台或重启。
旧 `site:pack`、版本内容上传及专属迁移已移除，没有新增数据库字段。正式页面和 Node 双入口装配已在 M1 完成，Docker 同端口 Nginx 分流已在 M2 完成隔离验证；
本轮未修改真实部署或用户数据库。


### 正式公开页面（v0.31.0 F01 M1）

`npm run build:bundle` 同时构建并装配 `public/site` 与 `public/admin`。默认 `npm start` 在连接数据库前
检查两套资源；`--api-only` 仅用于明确的 API 开发场景。完整模式 `/` 进入官网，`/download`、
`/changelog`、`/manual` 和 `/manual/:articleId` 可直接访问与刷新，后台仍为 `/admin/`。
未知 API、路径和缺失文件保留真实错误，不回落首页。没有部署内容目录时，官网显示内容尚未发布。

`GET /api/site/downloads` 返回各平台当前最新可用安装包；可选 `version` 查询固定版本。
点击下载前复核小清单，文件仍走原 installer 路径，由浏览器管理完成或失败；不会自动替换版本。
本地内容预览已使用正式页面布局，详见内容维护说明。测试示例留在隔离目录；正式首页配置与九篇初始手册已在 docs/site 维护，发布日志草稿不输出到静态目录。
