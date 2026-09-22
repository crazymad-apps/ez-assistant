# enterprise-center 模块约束

## 职责

独立 Node 24/TypeScript/NestJS/Fastify 企业中心，数据权威为 PostgreSQL 16.14。2026-09-18 用户确认业务数据访问改用 TypeORM（pg 驱动），复杂查询允许参数化 SQL；数据库升级继续使用 node-pg-migrate 的显式 SQL，不使用 ORM Schema 自动同步或第二套升级账本。遵循根 AGENTS、开发流程规范及前端编程规范的 TypeScript、测试组织、注释与最小新增原则；Desktop UI/MobX 条款不适用于服务端。

C01 M0—M3 已完成并获用户确认，包括 Admin 正式实现与同源装配；C04 M1 在此基础上实现模型代理。前后端独立工程，禁止跨工程导入源码，契约只通过 OpenAPI 生成；循环状态见 C01 开发计划。

## 数据与生命周期

- 只有一个 CENTER_DATABASE_URL，升级与日常业务共用本应用的数据库账号；不切换身份、不临时提权、不要求 PostgreSQL 超级用户。账号预先具有本应用专用库/schema 的建表、改表和读写权限。
- CENTER_DATABASE_AUTO_UPGRADE 默认 true；开启时 start 调用 node-pg-migrate 执行待完成 SQL，再检查版本并监听。关闭时只读检查版本，需要升级则退出，由 db:upgrade 手动执行。配置仅接受 true/false。
- 固定 public schema，不自动创建数据库或认领无账本的非空库；不维护逐字段、约束或索引快照。node-pg-migrate 的 pgmigrations 是唯一升级账本，不保留自研执行器或第二份版本表。
- 使用内置纯 SQL 文件，由框架排序、加锁、执行及记账；待执行业务 DDL 与记录同事务，失败停止并回滚。框架可能在首次失败后留下空版本表及其序列，这是非业务基础设施；不宣称这些对象也随业务事务回滚。已发布脚本不可修改。
- 首个 SQL 同事务创建中心身份、默认超级管理员 admin / 123456（只保存预生成 scrypt 摘要）及初始化审计；不另设初始化服务、命令或配置。重启不重新执行已记账 SQL，不重置管理员密码。
- 已有库升级前仍按项目安全规则停写、备份并恢复验证；开关不替代生产升级审批/备份。禁止删用户/改审计等业务约束由服务端 API 实现，不再宣称数据库账号无法执行这些操作。
- 不记录原始数据库错误、连接串、请求体、Cookie、Authorization 或响应凭据。对外错误使用固定原因码及服务端 request ID。
- 连接/查询/锁等待有界；升级/准入用维护 Pool 并在业务 DataSource 初始化前关闭，运行期间只保留一个业务池。关闭先停止 HTTP 接纳，再释放 DataSource；不把 Ctrl+C 生命周期绑定到其他产品进程。禁止 synchronize、migrationsRun、dropSchema 与扩展自动安装。
- Token 直接保存在进程内 Map<token,userId>，关联 LLM key 只映射到本次 Token；无 Session ID/表、Token 哈希、JWT、TTL、刷新令牌或 Redis。多 Token 并存，退出只删当前 Token/key，中心关闭或重启全部失效，限单实例部署。用户密码仍保存 scrypt 摘要。C03 M1 定向调整：本人改密和管理重置成功均保留已有 Token/key；启停/角色变化、显式退出、中心重启和事务结果不明仍按原规则撤销。
- 全局 src/security.ts 默认要求 Bearer，显式标记公开/可选认证/管理员接口；公开仍检查来源和 JSON 格式。Cookie 由前端处理，后端不读作身份、不设置、不清除。cl_ key 不能用于身份认证。Swagger 文档为明确公开的 Fastify 路由。
- 身份写入通过进程内串行段覆盖 QueryRunner 事务及提交后的内存凭据变更；业务与审计只使用该事务 Manager，密码计算在段外，段内重验。审计提交后才签发/撤销，失败不伪成功；提交结果不明清空凭据并失败，回滚也失败时关闭业务池，须排查后重启。M2 沿用同一串行边界和撤销口径，不增加跨进程锁或同步。

## 正式静态装配

- 两工程分别构建，build:bundle 编排构建及 assemble；装配到 mkdtemp 新目录，后台位于 public/admin，不写回源码、不覆盖旧制品，不复制 .env 或原型。C01 验证基本同源运行，正式分发流水线留 C06。
- 默认 main/start 要求正式静态资源，按 Vite manifest 校验后才允许数据库升级/连接和监听；仅显式 --api-only/start:api 可用于后端开发，不将缺少后台的运行称为完整产品。
- /admin/ 由 Center 同源提供，/api/ 仍走全局业务鉴权；后台静态入口公开，资源目录固定。未知页面/资源/API 不回落首页；隐藏文件及构建清单不可读，保持 no-store、nosniff 与后台 CSP。

## 验证

HTTP 契约由后端 Controller 与 DTO 的 Swagger 注解维护，统一导出 OpenAPI；Admin 由该文档生成类型及请求方法，不另写自定义 TS 导出体系。稳定 operationId、必填/可选字段、成功/错误结构与后续认证方式必须准确；注解不替代运行时校验。生成 openapi.json 不连接数据库，check 检查快照未过期；仅包含已实现接口，不写秘密或虚构业务。

tests/ 下维护 Vitest 单元及真实 PostgreSQL 集成验证，TypeScript/lint 均覆盖测试。生产代码不接收测试 SQL/故障注入。集成库必须显式指定且核实隔离环境，不使用用户既有库。破坏性测试先备份并验证；回滚测试也不得跳过目标核验。记录实际主机、端口、库、表、COUNT、备份及接口证据。未通过的步骤不得连续补救写库。

## 超级管理员

- 权限和保护只依据 users.is_super_admin，不绑定用户名、固定 ID 或初始化账号。允许运维直接在数据库赋予其他用户该标记；所有带标记且启用的用户具备管理员权限，即使 role=user。
- API 不提供授予/清除该标记的入口，创建/编辑中提交 is_super_admin 一律拒绝。禁止停用带标记用户或把其角色从 admin 降为 user，仍允许编辑显示名称、管理重置密码（包括自己）；个人改密始终校验原密码。不提供 DELETE。
- 数据库人工赋权属于运维操作，遵循项目数据库安全规则；产品不增加自动改写角色、触发器或只认一个超级管理员的逻辑。

## C04 M0 模型管理

`src/models/` 持有模型管理服务、发现/参数求值和模板加载。Provider、在线快照、固定参数与默认引用由 PostgreSQL 持久化；在线刷新全量成功才替换，空结果有效，失败保留。管理写入与审计共事务，网络及文件读取不占身份写锁，提交前复核当前身份和真实发现输入。

唯一初始 JSON 位于 `packages/assistant-protocol/resources/model-templates.json`，中心制品复制、个人 Runtime 编译嵌入。`CENTER_MODEL_TEMPLATES_FILE` 支持独立更新后显式重载；整份校验通过再发布，失败保留已有有效快照。只补 Unknown，不能用模板覆盖 Invalid/Unsupported 或固定参数；文件不携带可执行逻辑。

模型接口继续由 Swagger 生成前端 SDK，服务商凭据只写不回显。迁移 002 预建模型配置和调用追溯表；M0 只声明 `managed_models`，代理/配置测试/调用记录归 M1，Host 使用归 M2。部署维护入口见 [Center README](../../apps/enterprise-center/README.md)。

## C04 M1 代理与调用追溯

`src/llm/` 持有 raw HTTP 代理、调用 owner、索引与快照文件；身份和路由准入复用 IdentityService/ModelsService 的当前事实。管理 JSON parser 保留原限制，私有代理 scope 在收正文前鉴权、发送前重验；两协议字节透明转发，不重试、不跟随跳转。仅中心自产控制错误使用保留头，上游同名头剥除。

调用索引必须先于上游发送建立；网络结果与两侧快照独立结算，转发完成不代表模型任务成功。快照采集和待写共用内存预算，文件采用同目录不可覆盖发布；快照根独立于公开目录。恢复不猜测成功，清理先文件后索引且异常停止。关闭先取消在途模型/目录工作、等待记录收尾，再释放 HTTP 与 DataSource。具体资源限制及运维入口见 Center README。

管理端的调用查询、正文查看、策略修改和已保存配置测试由 Controller/DTO 生成 SDK；正文仅按需受保护读取，不作为静态资源。中心声明 `llm_proxy`；正式 Host 已通过统一模型配置来源接入，两协议继续共用既有模型业务与 Adapter。
