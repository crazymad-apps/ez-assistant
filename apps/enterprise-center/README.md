# Enterprise Center

C01 M3：企业中心后端，版本 `0.1.0`，Node 24、NestJS/Fastify、PostgreSQL 16.14、TypeORM 1.1.1（pg 驱动）、node-pg-migrate 9.0.0。
提供数据库升级、超级管理员初始化、协议信息、统一身份、用户管理、管理重置与审计查询；正式 Admin 独立构建后同源合并交付，没有 LLM 代理。M0—M3 已完成并获确认，C01 循环验收状态见[开发计划](../../docs/design/v0.27.0/cycles/C01-企业身份与中心管理基础/开发计划.md)。

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
制品包含后端 dist、package.json/lock、README/.env.example 以及 public/admin 静态页面。

在输出目录中：

```bash
npm ci --omit=dev --ignore-scripts
# 填写并核对该部署的 .env；已有库升级须先按项目规则备份、恢复核验和审批。
node --env-file=.env dist/main.js
```

只有一个 Node 服务，`/admin/` 与 `/api/` 同源；不需要 Vite、TypeScript、前端 node_modules 或额外静态服务器。
默认入口先依据 Vite manifest 校验首页、共享/延迟加载资源，缺失、空文件或清单无效时返回 ADMIN_ASSETS_INVALID 并在连接数据库前退出；这不是文件内容签名验证。
后台设置同源脚本 CSP、nosniff、no-referrer、禁止嵌入及 no-store；Ant Design 动态样式保留 inline style。
构建清单、隐藏文件和非后台目录不提供静态访问；未知资源和 API 返回 404，不回落首页。

仅后端开发使用 `npm run start:api` 或 `node --env-file=.env dist/main.js --api-only`；它明确跳过后台装配，不作为完整产品交付。

### 实时开发预览

后端在本工程运行 `npm run dev`，前端在 `../enterprise-admin` 运行 `npm run dev`，浏览器访问 `http://127.0.0.1:7310/admin/`。
前端 Vite 热更新并将 `/api` 代理到 `127.0.0.1:7320`。后端 `.env` 设置 `CENTER_PORT=7320`、`CENTER_PUBLIC_ORIGIN=http://127.0.0.1:7310`；连接已经核实的开发库时建议 `CENTER_DATABASE_AUTO_UPGRADE=false`，只检查升级账本。
后端 dev 使用 Node 24 原生 `--watch-path`（macOS/Windows），监听 src、tsconfig.json、package.json 与 .env；每次先完整编译和复制 SQL，再启动 API-only 服务，编译失败不运行旧代码。自动加载 `.env`，环境变量优先。Linux 不支持此 watch-path 入口，可使用显式构建加 start:api，后续按实际开发平台需求调整监听方案。
后端源码或配置变更会重启服务，内存 Token 随之失效，需重新登录；仅前端页面刷新会从 Cookie 恢复有效身份。dev 不是生产部署方式。

## 数据访问与升级边界

- 三张业务表通过 TypeORM Entity 映射，普通 CRUD 使用 Repository/QueryBuilder；用户/审计查询归属各模块 repository，复杂分页保留单语句参数化 SQL。API DTO 不继承 Entity，密码摘要默认不查询，身份核验显式读取。
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

| 接口 | 输入与返回 |
| --- | --- |
| POST /api/auth/login | `{username,password}`；200 返回 center_id、user、token 和 llm_key |
| GET /api/auth/me | 有效身份凭据；200 返回中心与本人身份，不再次返回秘密 |
| POST /api/auth/logout | JSON `{}`；204，仅删除当前 Token 和关联 key，重复或未知凭据幂等成功；仍检查传输来源 |
| POST /api/auth/password | `{old_password,new_password}`；204，验证原密码，更新本人密码并撤销本人全部 Token；管理重置任意用户使用下节独立接口 |

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

禁止通过 API 停用或降权超级管理员，不提供授予/清除标记或删除用户的接口。至少保留一个启用管理员。角色/启用状态实际变更、管理重置成功后撤销目标全部 Token；编辑显示名称不撤销，相同值不重复写入/审计。新密码统一要求 6—128 字符并包含英文字母和数字（512 UTF-8 字节以内）。

常见错误：409 USERNAME_EXISTS / SUPER_ADMIN_PROTECTED / LAST_ADMIN_REQUIRED、404 USER_NOT_FOUND、403 ADMIN_REQUIRED。管理重置与个人改密是不同接口，不根据目标是否为自己改变校验。操作成功后自身 Token 失效时，前端应返回登录页。

前端从 OpenAPI 生成请求方法和 DTO，后台入口按超级管理员标记或管理员角色判断，不只看用户名或 role 文本。正式页面和同源打包已在 M3 完成。

## 登录与全局鉴权

登录只提交 username/password，返回 center_id、user、token 和 llm_key。原 delivery、session_id、session_token 已删除，不保留未发布字段兼容。普通用户和管理员共用接口，管理 API 另行校验管理员权限。

Token 直接保存在单实例进程内 Map，不做哈希、不入库、无 TTL；多次登录互不覆盖。退出只删除当前 Token 和关联 key；本人改密撤销本人全部 Token；进程关闭/重启全部失效，需要重新登录。用户密码仍为 scrypt 摘要，持久用户与审计不受重启影响。

全局 [security.ts](src/security.ts) 默认要求 Authorization: Bearer；/api/info 和登录公开，退出可无效/无 Token 幂等调用，Swagger 文档明确公开。Cookie 不参与后端鉴权，后端不设置/清除 Cookie，前端负责保存/清除凭据。鉴权失效返回 401 TOKEN_INVALID。生产须 HTTPS，不将 Token/key 写日志或 URL；没有多实例共享登录能力。

LLM key 仅在内存关联本次 Token，不能调用身份 API。LLM 代理仍未实现。登录成功审计提交后才加入 Map；退出/改密审计失败时返回失败且不撤销凭据；提交结果不明时清空内存凭据并失败。身份写入串行到提交后的内存变更结束。

本次直接调整未发布的 001-initial.sql，不新增兼容升级脚本；旧试验库保留，不原地删表，新安装/验证使用新库。

## 配置

标准模板见 [.env.example](.env.example)，只列出实际支持的六个配置项。模板采用本机 HTTP 开发配置，数据库连接留空，必须明确填写；生产差异见文件注释。

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
| `CENTER_PUBLIC_ORIGIN` | 必填精确 origin，无路径/尾斜线 |
| `CENTER_ALLOW_HTTP_LOOPBACK` | 仅显式 `true` 允许本机开发 HTTP；非本地监听需 HTTPS origin |

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
