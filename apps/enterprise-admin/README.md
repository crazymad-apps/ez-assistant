# Enterprise Admin

正式管理前端使用 React、Ant Design 6、MobX、TypeScript 和 Vite，位于 `src/`。
原型位于独立 `prototype/`，其主题、布局及组件样式原样复用，不参与正式构建。
前后端独立工程；提供 `/admin/` 静态构建及 Vite manifest，Center 的 `npm run build:bundle` 负责配对构建并合并制品。M3 已完成并获用户确认；C01 循环验收状态见[开发计划](../../docs/design/v0.27.0/cycles/C01-企业身份与中心管理基础/开发计划.md)。

## 正式开发

使用 Node 24：

```bash
npm ci
npm run genapi
npm run dev
```

地址为 <http://127.0.0.1:7310/admin/>。`/api` 代理到本机 Center `127.0.0.1:7320`；
Center 的 `CENTER_PUBLIC_ORIGIN` 应设置为浏览器访问来源（默认 `http://127.0.0.1:7310`）。
开发 HTTP 还需显式开启 `CENTER_ALLOW_HTTP_LOOPBACK=true`。不把数据库密码或真实模型 key 配到前端。

身份与列表均来自实际 API，不含演示账户或本地成功审计。前端会话 Cookie 只保存后台 Token，
刷新先查询本人身份，成功后恢复原路由；退出调用服务端撤销当前 Token 后清理 Cookie。普通用户请通过 Runtime 登录，不能进入后台。
恢复期间不展示登录页或管理数据；网络失败保留凭据并提供重试，确认身份失效才清理。Cookie 使用 host-only、Path=/admin/、SameSite=Strict，HTTPS 加 Secure；名称包含端口避免开发实例互相覆盖，不承诺关闭浏览器后的长期记住登录。
Cookie 由前端读写，不能设置 HttpOnly；后端仍只认显式 Bearer，不引入 Session。密码、用户投影和 LLM key 不保存到浏览器存储。

`main.tsx` 在渲染前调用 `bootstrap.ts`，统一装配 Store、client 同源地址/超时、请求 Token 和响应失效拦截器。
显式请求 Token 仍绑定发起时身份，避免迟到请求跨账号；默认 Token 从当前 Store 读取，不放到 client defaults。
未知错误继续交给页面按读/写上下文展示，不全局重复弹提示或自动重试。没有引入参考项目的 SSO、激活或刷新令牌机制。

## 页面路由

使用 React Router 8.4.0 的 createHashRouter，路由表集中在 `src/app/routes.tsx`；PageLayout 只排布顶栏、侧栏、内容区和 Outlet，不读取业务 Store。其下 NavigationMenu 管导航与选中态，PageBreadcrumb 管路由标题，UserMenu 管账号菜单、账号弹窗、离开确认与焦点恢复；各自样式就近归属，不再维护 page/setPage。
入口分别为 `/admin/#/login`、`/admin/#/users`、`/admin/#/audit`、`/admin/#/audit/:id`。菜单/面包屑、前进/后退跟随路由；未知地址显示 404。
刷新后先恢复身份，凭据失效时才重新登录，再回到请求的本地页面。审计详情通过现有 GET /api/audit 的 id 筛选独立读取，历史记录只含路径，不包含审计正文或凭据。
弹窗维持原型 UI，编辑离开与提交中导航由 NavigationGuard 的唯一 useBlocker 管理；账号菜单和用户页面仅登记离开回调，表单状态仍在各自组件。URL/hash 路由不需要 Center 增加 SPA fallback。
useBlocker 只保证路由器管理的站内导航；手工修改地址栏 hash 或关闭/刷新页面不属于此拦截保证，不伪造浏览器历史来实现另一套路由。

## 代码格式

`.prettierrc` 与参考项目 `zx-zhkf-admin` 一致：2 空格、120 列、单引号、尾逗号。
Prettier 固定版本并作为本工程开发依赖；仅格式化正式手写源码、测试和工程配置。
原型、生成 SDK、锁文件和构建产物忽略；视觉沿用原型与用户已确认调整，不增加样式逐字比较测试。

```bash
npm run format
npm run format:check
```

`npm run lint` 包含格式检查，避免后续重新出现密集单行代码。

## 接口生成

按用户指定的 `zx-zhkf-admin/genapi.js` 采用 Hey API + Axios 插件，路径、方法、请求与返回类型
从 Center OpenAPI 生成，不手写另一套 DTO 或修改生成代码。

```bash
# 默认读取 ../enterprise-center/openapi.json，写入 src/request/openapi
npm run genapi

# 可切换在线 OpenAPI；地址以 Center 实际配置为准
OPENAPI_INPUT=http://127.0.0.1:7320/api/openapi.json npm run genapi

# CI / 构建检查生成结果是否与已保存契约一致
npm run genapi:check
```

`OPENAPI_OUTPUT` 可覆盖输出目录；业务默认导入 `src/request/openapi`，改变目录需同步消费者。
更新后端 DTO 后先在 Center 执行 `npm run openapi:export`，再在本工程生成并检查差异。
Axios client 随当前生成器输出，无需另外安装独立旧版 client 包。
TypeScript 固定 6.0.3：当前生成器依赖旧编译器 API，尚不能直接使用 TS 7 原生编译器。

```bash
npm test
npm run lint
npm run build
npm run preview
```

正式产物在 `dist/admin/`；preview 只用于本地检查构建产物，不替代 Center 生产部署。
原型的 dev/build 入口继续独立保留。测试只覆盖核心身份/查询竞态、关键写入、失败恢复与秘密排除；
单元测试不等同于真实数据库和浏览器全链路验收。

## 保留的交互原型

原型仅供 C01 页面评审：React + Ant Design 6 + TypeScript + Vite + Sass。
代码和演示数据在 `prototype/`，不发起后端、数据库、Host 或模型请求。
所有操作仅修改浏览器内存，刷新或“重置演示”恢复数据；不要输入真实凭证。

```bash
npm ci
npm run dev:prototype
```

预览地址：<http://127.0.0.1:7310/>，只监听本机。默认进入模拟管理员首页，侧栏可查看登录页。
公开演示账号：`admin`（超级管理员）、`lin.zhiyuan`（管理员）、`chen.yu`（普通用户，无后台权限）；初始演示密码均为 `Demo@12345678`，不是产品默认凭证。

用户管理支持内存中的搜索、筛选、分页、新建、编辑和启停。管理员重置密码无需原密码；本人账号修改密码需要原密码。模拟审计可筛选和查看详情；底部可切换空数据及加载失败状态。

```bash
npm run test:prototype
npm run lint
npm run build:prototype
```

单元测试仅覆盖演示模型。界面需另行在真实浏览器中检查；上述命令不验证正式认证、安全、会话、审计或数据库。
包版本已随正式前端设为 `0.1.0`，与 Center 配对，不改变 Runtime 版本；这不表示 C01 已验收或发布。
框架决策与原型边界见 [C01 UI 框架决策与原型](../../docs/design/v0.27.0/cycles/C01-企业身份与中心管理基础/UI框架决策与原型.md)。
