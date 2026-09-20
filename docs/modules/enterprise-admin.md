# enterprise-admin 模块约束

## 职责与当前阶段

独立 React/TypeScript 管理前端；正式后端属于 `apps/enterprise-center/`，两个工程独立依赖与构建，同源合并交付。
2026-09-18 用户授权开始正式前端应用，组件和样式参照现有原型，不随意调整 UI；2026-09-20 C01 M3 已完成并获用户确认。prototype/ 仍独立保留，正式入口使用 src/；循环状态见 C01 开发计划。

修改前阅读根 AGENTS、开发流程规范、前端编程规范、[`企业后台前端开发规范`](../specs/企业后台前端开发规范.md)（状态架构与请求层）、[`企业后台主题Token设计`](../specs/企业后台主题Token设计.md)（视觉 Token 清单与扩展规则）及 C01 设计。

## UI 方案

- 页面布局统一为 app/PageLayout，只承载区域排布与 Outlet。NavigationMenu、PageBreadcrumb、UserMenu 是其私有子组件，分别承载路由导航、面包屑、账号菜单及账号弹窗生命周期；不将业务状态上提到主体布局，不为纯组合布局添加 observer。
- 正式沿用原型 React + Ant Design 6、主题 Token、布局、表格及弹窗样式；新增真实异步状态不构成重设计授权。Store 仅为身份与写入权威（当前用户、内存 Token、退出确认、写入锁），文件尾单例导出、组件直接 import，不经 props 或路由参数透传；页面列表、筛选参数和加载态是路由页面临时态，留在页面级 hook（PagedList + useState），不进入全局 Store。
- 这是管理前端专属的成套组件库试用决策，不修改 Desktop 的 SelectionPopover、样式或依赖。
- 选择、表格、表单、弹窗统一用 Ant Design，不复制 Desktop 原语，不引入 Ant Design Pro/Umi。
- Ant Design 内部样式机制属于库边界；业务布局使用 Sass Module，不再引入第二个 CSS-in-JS 库。
- 通过 ConfigProvider Token 调整颜色、字号和控件高度；不深入覆写库内部 DOM 选择器。
- 保持前端规范的严格 TS、组件拆分、语义命名、中文边界注释和无障碍要求。
- 页面导航统一由 React Router 路由表与 Outlet 承载，不维护 page/setPage 或详情 selected 条件分支。采用 createHashRouter 保持 `/admin/` 静态部署；身份守卫只控制展示，服务端仍鉴权。弹窗草稿留组件，不进入 URL/history；提交中/编辑中站内导航通过 useBlocker 处理，审计详情按 ID 查询而非依赖当前分页内存。
- 正式手写代码统一执行 Prettier；配置与用户指定 zx-zhkf-admin 的 .prettierrc 一致（2 空格、120 列、单引号、尾逗号）。format:check 纳入 lint；原型、生成 SDK、锁文件和产物不由该格式命令改写。

## 原型隔离

- 原型代码与演示数据仅位于 `prototype/`，不得进入未来正式 `src/` 或公共协议；这是用户明确要求的模拟入口，不是生产代码中的 mock。
- `dev:prototype`、`build:prototype` 等名称必须明确；不提供把原型伪装为正式管理端的默认发布命令。
- 仅浏览器内存状态，刷新复位；不连接后端、数据库、Host、模型服务，也不写 localStorage、Cookie 或 IndexedDB。
- 测试统一放 `tests/`。演示账号密码是公开测试输入，页面注明不得输入真实凭证，不写日志或持久化。
- 未决超级管理员保护不由原型确立；相关动作显式提示未模拟，不标为真实权限限制。
- 原型静态服务仅监听 loopback，不为预览开放局域网。

## 验证与交接

NavigationGuard 在已登录身份下维护唯一 Router blocker，账号菜单/用户页仅登记离开回调；表单状态不进入布局或全局 Store。新密码前后端统一至少 6 位、含英文字母和数字，保留 128 字符/512 UTF-8 字节上限；登录旧密码不套用新策略。

正式前端测试按用户 2026-09-20 要求收敛到核心身份、权限、关键写入、竞态和失败恢复；不新增源码结构扫描、CSS 逐字比对、静态文案快照或框架行为测试。具体遵循企业后台前端开发规范，不影响原有 UI 设计约束。

按改动范围执行类型检查、对应入口构建、核心测试和必要的真实浏览器操作；只在现有计划保留完成结论、未完成项与风险，不要求截图或验证数据归档。前端模拟通过不代表安全、认证、审计、并发、数据库或跨循环验收通过。
用户确认原型后再按开发计划建设正式入口；不得把模拟权限或演示数据接入正式部署。

## 正式请求与凭据

- 按用户指定参考项目 genapi.js，使用 @hey-api/openapi-ts 与 @hey-api/client-axios；genapi.js 支持 OPENAPI_INPUT/OPENAPI_OUTPUT，默认读取 ../enterprise-center/openapi.json，输出 src/request/openapi。只由生成器写入，不手改生成产物。
- 按同项目 bootstrap.ts 的启动组织方式，渲染前集中配置 client、默认 Token 注入和身份失效拦截器，并从后台 Cookie 恢复 Token、查询本人身份；显式请求 Token 保留发起时身份，失效只能清理匹配当前 Token 的投影。拦截器随 bootstrap dispose 释放，不重复注册；不复制参考项目 SSO 或激活机制。
- 原型的同步回调改为真实异步 API；同一身份、同一查询的最新请求才可提交 UI 投影（页面级 PagedList 代次守卫）。身份切换由路由守卫整树卸载清空页面状态、关闭表单并丢弃迟到响应；取消请求不表示服务端回滚，不自动重发写入。新增/编辑/启停等写入成功后由页面直接调用列表刷新回调重载，Store 不感知列表。
- 2026-09-20 用户确认：前端以会话 Cookie 保存后台 Token，刷新先恢复凭据并查询本人身份，确认前不渲染登录页或管理数据；网络故障保留凭据、原路由并提供重试，确认失效或退出成功才清理 Cookie。本人改密等已确认撤销同样清理；退出失败遮蔽业务数据并允许重试。应用销毁只清内存，不删除 Cookie，React effect 清理不得销毁应用 Store。
- Cookie 使用 host-only、Path=/admin/、SameSite=Strict，HTTPS 时加 Secure；名称包含端口，避免同主机多个开发 Center 相互覆盖（并非安全隔离）。使用会话 Cookie，不承诺关闭浏览器后的长期记住登录。前端读写不能使用 HttpOnly，必须保持既有 XSS 防护；后端仍只读取 Bearer，不新增 Session 或 Cookie 鉴权。清理时比对 Cookie 中的 Token，避免旧标签页删除新登录凭据。
- 仅 Token 允许以上 Cookie 保存；用户投影、密码、LLM key 不落浏览器存储，也不记录秘密日志。后台不用 LLM key，不保留该返回值，不使用 localStorage/sessionStorage。
- ORM Entity 不进入前端；服务端权限、分页、过滤与审计为权威，禁止按当前分页模拟最后管理员保护或生成本地成功审计。
