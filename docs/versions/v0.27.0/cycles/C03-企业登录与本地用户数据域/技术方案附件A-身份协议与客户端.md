# C03 技术方案附件 A：身份、协议与客户端

本附件属于[技术方案](技术方案.md)，已于 2026-09-20 随主方案获用户确认；M1 身份部分已完成并获用户确认，M2 用户 Runtime 已完成并获确认，M3 资源隔离已获用户确认，M4 正式客户端已获用户确认，M5 集成验收已获用户确认，C03 待提交。只定义 C03 身份传输和客户端边界，模型托管不在本附件实现。

## 一、身份与凭据的最小表示

凭据按通信方向区分，不能把“企业模式下的 Web 登录”误称为浏览器持有企业后端 Token：

```text
用户 Web/Desktop ── Host Token ──> Runtime Host
                                   ├── Center Token ──> 企业后端身份 API
                                   └── LLM key ───────> 企业后端模型代理（C04）
```

Host Token 由 Host 签发，用于客户端访问 Host；个人和企业模式共用签发、保存、传输及过期机制，区别是 Host 内关联个人域还是已验证的企业用户域。Center Token 由企业后端签发，仅由 Host 持有并用于访问该后端，不发给用户 Web。LLM key 也是 Host 持有的后端凭据，其用途限于模型代理，不能当身份 API Token 使用。

Center Token 与 LLM key 沿用当前实现的不设 TTL 规则，不会按时间自然过期，没有刷新令牌或续期接口。LLM key 只映射到关联 Token，没有独立有效期；Token 被撤销时 key 一同失效，Center 关闭/重启会清空二者。C03 M1 已按第四节调整本人改密和管理重置，成功后保留已有凭据；异常提交结果不明时的凭据清理也保留。客户端 Host Token 的七天期限不适用于这两种后端凭据。

复用 Host `access/credentials.rs` 的登录表、`LoginSession`、`AccessPermit` 和随机 Token 生成，不另建企业 Web Token 类型、登录服务或持久表。

| 对象 | 归属与内容 | 不允许承载 |
| --- | --- | --- |
| 本机 bootstrap | 当前 Host 实例、本机管理资格 | 企业用户身份、企业会话数据读取权 |
| Host 普通 Token | 既有内存登录表的索引，关联稳定用户键及客户端兼容声明 | 固定某次 Center 登录、跨 Host 使用或调用中心 API |
| 用户当前企业状态 | Host 按稳定用户键保存的一份 Center Token、LLM key 及已验证身份；新登录成功时整体替换 | 密码、本地持久表、任务级凭据映射、普通日志序列化 |

统一状态的范围是“同一 Host 的同一用户”。Desktop、Web、设备输入和后台任务共用它；不同用户仍严格隔离。同用户多次登录不形成多套后端执行身份，旧客户端有效 Host Token 继续关联同一用户，所有后续中心调用读取当前状态。Host 不保存各次登录历史，也不在任务中保留某次登录的 key。

个人和企业 Host Token 均沿用既有七天期限与摘要查找；它们只控制客户端访问。Host Token 到期关闭对应传输/PTY，不清除仍被其他客户端或后台任务使用的用户状态，不撤销 Center 凭据或取消任务。最后一个有效 Host Token 过期且后台工作结束后，按附件 B 回收用户服务及不再使用的内存凭据；不因此额外调用 Center logout。

同用户可以有多个 Host Token，但它们读取同一当前企业状态；任一端主动退出结束该用户在本 Host 的统一状态，全部相关 Host Token 失效。64 个 Host 登录记录的既有上限继续生效，签发前清除已失效记录，达到上限返回 busy，不驱逐仍有效的登录。Center 自身仍允许多 Token 并存，后台或其他 Host 的登录不受这个本机状态合并影响。

## 二、Center 接入及身份核验

### 2.1 接口复用

Host 用已有 `reqwest` 和集中配置的中心地址访问：

| Center 入口 | Host 用途 |
| --- | --- |
| `GET /api/info` | 核对 `protocol_version` 与 `min_protocol_version`；不比较 Center 和 Runtime 软件版本相等 |
| `POST /api/auth/login` | 转交账号密码，获得稳定身份及本次 Token/key |
| `GET /api/auth/me` | 在线核验 Center Token、中心与用户身份是否仍匹配 |
| `POST /api/auth/password` | 本人改密，保留当前登录的完整身份约束 |
| `POST /api/auth/logout` | 撤销当前 Center Token 及关联 key |

Center DTO 在 Host enterprise 适配模块私有反序列化，应用协议只公开客户端实际需要的字段，不把 Nest Entity 引入 Rust 或共享 Protocol。不得把 Center 返回 JSON 不经校验直接当作 Host 身份。

请求禁止自动重定向，防止带凭据跨目标；普通认证请求总超时 5 秒，建连 3 秒，登录/改密总超时 15 秒；响应 JSON 上限 64 KiB。请求错误脱敏，不返回 Center 的原始响应或 URL 中的秘密。本版不另设中心客户端并发配额。

企业配置中心地址不允许 userinfo、fragment 或携带查询 Token。支持配置的 HTTP/HTTPS，不把主机名当成中心身份。中心来源和端口由本机管理配置确定，普通用户不能提交任意 Center URL。

### 2.2 登录顺序

1. 在现有网络准入和兼容检查后，进入企业登录分支；失败认证不分配用户目录。
2. 检查中心企业协议，然后只执行一次 login。网络响应不明时不自动再发 login，用户显式重试可能产生另一个合法中心 Token，不能假称第一次未生效。
3. 校验返回的 UUID、正整数 user.id、启用状态及凭据用途。用户归属只由这些服务端事实决定。
4. `host.toml` 尚无中心身份绑定时，在 Host 配置写 gate 内保存首次成功认证的 `center_id`；已有绑定必须相同。并发首次登录重新读同一配置再比较，不能最后写入覆盖另一个中心。
5. 确认 Host 未关闭、身份匹配且可签发 Host Token 后，在该用户状态写入边界内整体替换当前 Center Token/key，登记关联用户键的 Host Token/Cookie，返回登录成功，不等待 Runtime 初始化。“最新”指最近一次成功提交到 Host 的登录结果；失败登录不覆盖原有效状态。同用户并发登录按成功提交顺序生效，不增加请求序号或任务版本字段。
6. 登录未能提交时，向中心 best-effort logout 清理本次新取得但未采用的凭据，不清理原有效状态；快捷兑换只签发客户端访问凭据，不重新向 Center 登录、不替换用户当前企业状态。
7. 客户端取得 Host 凭据后独立调用 `POST /runtime/ensure-ready`，等待期间显示 loading，成功后进入业务。初始化失败允许重试，不清除已提交的用户登录状态或 Host Token，也不能静默新建替代库。

登录与用户服务就绪是两个独立结果。就绪接口也用于刷新、重连或用户域回收后的再次登录，个人与企业共用，具体见附件 B；服务仍在则直接成功，已回收则重新装配，不轮询初始化状态。Center 网络请求的 15 秒超时只约束对应中心调用，不把它机械套在本地数据库初始化上；Host 停止、请求超时或初始化失败使用现有取消/收尾路径。

中心 identity 不匹配时 `center_identity_mismatch`，用户需通过本机管理明确切换；普通重登不能批准中心变化。

### 2.3 当前状态的核验与并发

新受保护请求先校验客户端 Host Token，再读取该用户当前企业状态并核验 `/me`；同一当前凭据的并发核验共享 future，不增加授权缓存 TTL。认证差异只在 Host 接入层处理，普通 handler 只取得用户服务句柄，不接收执行授权参数或判断企业模式。个人来源复用本机认证；Host 管理仍走 bootstrap。

有活动连接或后台工作的用户，沿用 Host Supervisor 的一个复核循环：最多间隔 30 秒启动核验，5 秒超时；没有活动时不轮询，下次请求再验。复核对象是用户当前凭据，不是每个端或每个任务。Host Token 全部过期但后台工作仍在时继续复核。

- 核验成功：确认中心及用户身份不变，更新显示名称等投影。
- 明确 401、停用或身份变化：若结果仍对应当前凭据，结束该用户统一状态，所有本机客户端失效，该用户工作受控取消；不影响其他用户。
- 网络/503/超时：返回 `center_unavailable`，在既定最长 35 秒检测窗口内关闭该用户活动连接并停止当前工作。保留 Host Token 和当前凭据供恢复后重验，不能宣称 Center 已撤销。重验成功后可恢复访问，已取消工作仍须显式恢复，不自动重放。

凭据是可整体替换的 Host 私有内存值；网络请求只短暂取得发送时的快照，不持有同步锁等待网络。结果提交时比较是否仍是当前值：旧凭据的晚到核验或错误不能清除、覆盖新登录；必要时在本次请求时限内重新核验当前值。无需新增 generation 或身份版本字段。

本节的身份失效结论来自中心身份契约。LLM 代理的配置过期、上游 Provider 认证错误或网络失败不能直接当作用户身份失效；须区分中心控制错误与上游错误，旧 key 的拒绝也不能撤销新登录。配置刷新及模型调用失败的交接约束见[附件 B 第 2.3 节](技术方案附件B-用户Runtime与工具隔离.md#23-中心配置变化与请求被拒绝c04-交接草案)。

登录发布、用户退出和失效处理在同一用户操作边界内串行提交；退出先关闭当前 Runtime 的业务准入并清除用户状态及相关 Host Token，再异步收尾。后续新登录可以建立新状态，但就绪接口须等待旧 Runtime 关闭；旧收尾只处理捕获的旧资源，不清理新凭据。密码登录、身份查询、改密、退出不依赖 Runtime 初始化；退出不先等待 `/me`，以免中心不可达阻止本机退出。

## 三、Host HTTP 与应用 DTO

在现有 `host_access.rs`、Host 启动投影和 capabilities 上扩展，不增加新的业务协议包。以下字段名为本方案拟定契约，实施时使用 Rust 定义并生成 TS。

| 入口 | 契约 |
| --- | --- |
| `GET /capabilities` | 发布 `personal/enterprise` 模式、企业登录能力及所需客户端版本；公开阶段只含登录所需信息，不含其他用户、路径或 secret |
| `POST /auth/login` | `HostLoginRequest` 增加 `Enterprise { username, password, native }`；与个人分支一样，Web 设置 Host Token Cookie，原生客户端显式选择 Token 响应；native 只选择响应形式，不代表管理权限 |
| `POST /auth/login` 的 `Token` 分支 | 两种模式均将快捷 Host Token 兑换为浏览器 Cookie；企业在 Host 内关联同一用户当前状态，不重新登录 Center |
| `POST /auth/login` 的 `Desktop` 分支 | 企业必须有用户 Token 才能签发快捷凭据；bootstrap-only 拒绝业务凭据签发 |
| `GET /auth/session` | 返回已完成登录的种类、既有到期时间、instance_id、本人身份及登录上下文；不返回 Center Token/key，不增加 user_domain 初始化状态 |
| `POST /runtime/ensure-ready`（新增） | 普通 Host 认证后等待本人用户域初始化完成，已就绪则直接返回；成功为 204，无请求体或就绪状态 DTO，失败返回具体错误；个人与企业共用 |
| `POST /auth/password`（新增薄路由） | `{ old_password, new_password }`，仅企业登录；转交 Center，成功不清除本机登录 |
| `POST /auth/logout` | 结束当前用户在本 Host 的统一登录状态，使该用户各端 Host Token 失效；企业另撤销当前 Center Token/key |
| HostAccess 命令 | 增加运行模式/中心配置字段及受控配置意图，复用 expected_revision 和本机管理校验 |

`HostLoginResult` 只在实际签发且调用方选择原生 Token 响应时包含 Host Token；Web 经 Set-Cookie 接收，身份查询不能重复吐出 Token。登录结果不以用户域初始化为前提。身份 DTO 包含模式、中心 ID、用户 ID、账号和显示名称等所需字段；不新增本地账号实体，不公开用户域列表或初始化状态。

所有带身份响应 `Cache-Control: no-store`；两种模式的 Web 都沿用 Host HttpOnly Cookie、SameSite/Origin 校验及 HTTPS 下 Secure 属性，原生客户端继续使用 Bearer；无效显式 Bearer 不回退 Cookie。模式切换会重启 Host，原实例登录随之失效，残留旧 Cookie 不能获得新实例身份。

认证与准入错误区分 `authentication_required`、`center_unavailable`、`center_identity_mismatch`，HTTP 状态分别为 401/503/409，在边界统一映射；既有 Host 登录表满额仍沿用现有 busy 映射。初始化失败复用现有启动/存储错误分类随就绪请求或实际触发装配的业务请求返回，例如 `storage_unavailable`，不增加表示“正在初始化”的业务错误，也不将初始化失败当作登录失效。普通业务继续使用既有 Runtime 错误：C03 中心模型配置未就绪使用 `configuration_unavailable`，无权修改托管模型使用既有 `operation_not_allowed`；不增加按企业模式命名的业务错误。共享 Cookie 已换登录而旧页面仍发请求时，返回 `login_context_changed`（409），处理见第 5.1 节。

## 四、退出、改密与 C01 重开

Desktop/Web 的主动退出入口先共用确认流程，显示当前账号、Host 和[功能设计第 4.3 节](功能设计.md#43-登录并发和退出)约定的“中断所有任务、使其他端退出”文案。只有用户点击“中断所有任务并退出”才调用 `/auth/logout`；取消或关闭确认框不清 Cookie/原生 Token、不取消请求和任务。更换账号先通过退出登录完成该流程，不另设绕过确认的退出入口，也不根据客户端任务快照推断“无任务”而跳过确认。此处是客户端交互，不增加后端确认票据或任务清单协议。

用户从任一端显式 logout，Host 结束该用户的统一状态：关闭当前 Runtime 准入，清除该用户所有 Host Token，关闭其事件流、上传下载和 PTY，沿用 Runtime 取消/关闭流程停止本人域的工作，并单次调用 Center logout 撤销退出时捕获的当前 Center Token/key。其他用户、其他 Host 及 Center 后台登录不受影响；不增加全中心“退出所有设备”接口。

即使 Center 不可达，本机也不恢复已结束状态；响应区分 `local_ended` 与 `center_revocation_confirmed`。用户 Runtime 的受监督关闭和资源回收不以中心撤销成功为前提，磁盘数据保留；认证结果也不等待用户库初始化或资源关闭。重复退出沿用既有幂等语义。

快捷登录和独立密码登录都汇入同一用户状态，所以任一端确认退出会影响这个用户在本 Host 的全部端和任务。关页面、断连和单个 Host Token 自然过期不会触发这种统一退出。确认流程仅用于用户主动退出；凭据已失效、账号停用等被动失效仍立即按认证规则处理，不能等待用户确认后才拒绝访问。

新的账号密码登录只替换 Host 当前使用的凭据，不追溯撤销被替换的中心凭据，也不打断已发出的请求；后续调用只取最新值，不保留可回退的旧凭据池。Center 的多 Token 与当前 Token logout 规则保持不变。

对 C01 定向重开的实施范围：

1. `IdentityService.changePassword` 和 `UsersService.resetPassword` 保留原有验证、密码哈希、身份写串行段和审计事务，只移除密码提交成功后 `revokeUser` 的回调。
2. 登录/改密并发仍复核密码快照；新登录不能用修改前的密码计算结果越过提交边界。改密失败不会更改密码或凭据。
3. `users/service.ts` 的启停、角色变更撤销保留，logout 仍撤销当前 Token/key；数据库提交结果不明的 fail-closed 处理也不借本次需求取消。
4. 修改两种密码 API 的 Swagger 说明、对应 OpenAPI 生成物及 Admin 成功处理：改密/重置自己后显示成功，不清 Cookie、不强制跳登录；密码输入值及时清理。
5. 重验多 Token 与关联 LLM key 的 `/me`/`llmIdentity` 可用性、旧/新密码登录、改密竞态、审计失败及停用/角色撤销。C03 的确定性 Host 任务跨密码变更继续下一步；真实模型代理在 C04 再验。

不修改用户 Schema、初始 SQL、密码策略或 Center Token 存储机制。原 C01 归档文件保持历史事实；实施计划显式标注重开范围和复验结论，不能声称原归档已验证此新行为。

## 五、统一 Host Web 登录与 Desktop

### 5.1 凭据存放与页面恢复

Desktop/Web 登录页使用紧凑表单，去掉主标题、副标题及装饰性页脚，不提供“管理本机 Host”入口；Host 管理仍由登录后的设置或 Client 进入，企业设置不显示个人访问密码。登录后用户名展开账号菜单，只提供修改密码、退出登录；改密只打开密码表单，提交按钮与关闭按钮并排置于 footer；退出先展示任务中断确认。更换账号通过退出后重新登录完成，不提供重复菜单项。

- 企业与个人 Web 统一使用 Host 签发的 HttpOnly Cookie，不在 sessionStorage/localStorage 保存 Token。页面刷新沿用 `/capabilities` 与 `/auth/session` 恢复 Host 连接；企业身份和用户域由 Host 在服务端关联。
- 登录成功或恢复已有登录后，统一 RuntimeClient 调用 `/runtime/ensure-ready`，等待期间显示 loading，成功后读取业务投影；错误可重试，只有凭据失效才返回登录。该步骤独立于登录请求，不要求前端维护用户域状态机。
- 同一浏览器配置、同一 Host 来源的标签页共享当前登录；新开或复制标签页沿用该 Cookie。登录、退出、切换账号对这些页面共同生效，不支持这些标签页同时保持不同账号。用户已明确选择此简化行为。
- Host 仍允许来自不同浏览器、浏览器配置、设备及 Desktop 的多个用户同时登录；同用户各端共用 Host 当前登录状态，不同用户分别维护。
- Center Token、LLM key、密码、bootstrap 不进入浏览器存储或 Web 响应；Cookie 内仅为 Host Token。继续使用现有 CSP、受控渲染和同源保护，不新建企业 Web 凭据机制。
- Host Token 无效或过期时进入登录页，由 Host 清除失效 Cookie；网络失败保留 Cookie、遮蔽旧业务投影并允许重验，不能因不可达宣称凭据已经撤销。用户业务投影不持久缓存。
- Desktop 用户 Host Token 只留原生受控连接状态/受信 WebView 内存；刷新可从该连接恢复，退出桌面后企业需重新登录。本机管理 bootstrap 独立保留，不当成企业用户 Token。

同源页面通过不含凭据的登录变化通知触发重新读取 `/auth/session`，页面恢复可见及重连时也重验；通知只是提示，不能直接携带并切换可信身份。Cookie 被新登录替换时，只结束被替换的浏览器 Host 登录及其连接，不因此取消该用户后台任务；同用户重新密码登录只更新 Host 当前凭据；用户明确执行 logout 则仍按第四节撤销。

为防止旧页面表单在 Cookie 已切到 B 后被当作 B 的新操作，复用既有 `LoginSession.id` 生成不可作为凭据使用的 `login_context`，在登录/身份结果中返回。页面请求携带它作为“期望仍是本次 Host 登录”的校验值；Host 先验证 Cookie，再比较登录上下文，不匹配则拒绝执行并要求页面清理旧投影、重建连接。它不是另一个 Token，不能单独授权，不含 Center 凭据，不新增计数器、持久表或有效期；两种 Web 模式共用。普通请求使用 header，浏览器直接资源请求使用非秘密查询字段，WS 使用首帧字段；原生 Bearer 已固定请求身份，可沿用原路径。未登录查询及 login 本身不要求旧上下文。

Center Admin 仍使用它自己的后台 Cookie，与 Host Web Cookie 是两个应用的认证，不能混用。

### 5.2 已登录快捷打开

已登录 Desktop 从当前用户连接调用既有快捷凭据签发，原生层只允许打开当前 Host 的 URL，携带普通 Host Token fragment。页面在任何业务请求前调用现有 `takeWebLoginToken()` 清除 fragment，再向同 Host `/auth/login` 兑换 HttpOnly Cookie。两种模式走同一流程；无效快捷 Token 明确失败，不能用现存 Cookie 悄悄当作兑换成功。企业不把 Center Token 或 LLM key 带到 URL。

快捷登录成功后，该浏览器同源页面共同使用兑换后的 Host 登录；已有页面按第 5.1 节清理和恢复，不能承诺新页面换账号而其他同源标签页保持旧账号。快捷兑换只关联源 Desktop 的同一用户，后端请求使用 Host 当前凭据。

原生 `open_runtime_web` 需从当前用户连接获取凭据，而非无条件从本机 bootstrap 取凭据；仅管理 Host 的 Client CLI 没有企业用户登录上下文时打开普通企业登录页。当前 Client 不因此新增企业账号密码保存功能。

### 5.3 业务传输与资源

两种 Web 模式都复用 RuntimeClient 的 `credentials: same-origin`、Cookie SSE/WS 和既有预览/下载入口；WS 保留有界首帧、来源与客户端兼容校验，身份仍从 Cookie 取得。Desktop 继续用 Bearer 和受控原生下载。服务端认证后选择用户域，不能因为浏览器使用相同 Cookie 机制就共享用户数据。

图片、缩略图、导出及浏览器原生流式下载使用既有 Cookie 认证，不为企业新增 Token 表单下载接口，也不要求整文件读入 JS Blob。需要 Blob 的现有预览沿用原限制并在清理时 revoke object URL。所有请求及响应流仍校验资源归属并受登录访问失效保护；URL 不携带 Host Token、Center Token 或 LLM key。

响应提交使用当前 RuntimeClient 对象身份和 AbortController；切换用户销毁旧 client、投影 Store、预览 URL、订阅及终端。复用 C02 可靠投影的 owner 校验，不额外持久化客户端 identity revision。

登录后的普通页面只使用统一 RuntimeClient、投影 Store 和命令，不按 mode 分成个人/企业页面。模式信息只用于登录/本人账号/Host 设置，以及明确约定的模型管理与默认模型选择入口；其他功能按真实资源状态展示，不从企业标签推导隐藏规则。

## 六、关键验证点

- 无企业身份的 bootstrap 只能操作本机 Host 控制，不能列用户库、读取会话或签发企业快捷 Token。
- 不同浏览器配置 A/B 与源 Desktop 并发、快捷打开，验证各自身份、事件、预览、下载、WS 和刷新；同源标签页验证共享 Cookie、共同切换及旧页面请求被拒绝，迟到响应不串账号。
- 同用户 Desktop/Web 再次密码登录更新唯一当前凭据，各端及后台任务的后续调用使用最新值；任一端主动退出使该用户本 Host 的其他端一并失效，其他用户和其他 Host 继续。
- Desktop/Web 退出明确展示账号、Host、全部任务中断和其他端退出的影响；取消/关闭确认框不发送 logout、不清凭据、不影响任务，确认后才执行。活动任务不在当前页面或页面投影不完整时也不能省略提示。
- Center Token/key 不因时间流逝失效，二者共用撤销及进程生命周期；Host Token 仍按自身期限到期，不能混用。两种密码操作保留凭据已在 M1 实现并验证，不能沿用 C01 归档时的密码撤销断言。
- 主动退出或最后一个 Host Token 过期后，无有效 Host Token 且无工作需要运行时回收用户服务，再登录可通过就绪接口恢复数据；过期不会取消已有后台工作，工作结束后再次检查回收；其他有效 Host Token 仍在时自然过期不回收；主动退出统一清除该用户全部 Host Token，中心不可达时本机仍可完成清理。
- 两种模式的 Host Token 签发/过期机制一致；企业 Host Token 过期后页面需重新登录，已接纳任务及后续模型/工具步骤继续使用 Host 当前有效企业状态，不能只验证登录表状态。
- 登录不等待用户域初始化；就绪接口等待期间前端 loading，初始化失败保留有效凭据并可重试，已就绪则直接返回。同用户并发就绪调用复用同一 Runtime，不轮询初始化状态。Token 满额、中心身份变化、Center 响应超时和退出结果不明均有明确错误。
- 旧凭据核验与新登录/退出交错时不覆盖当前状态或复活已失效凭据；中心临时不可用不伪造全局 logout，不自动重放已接纳任务。
- 个人 Cookie、快捷登录、远程访问开关和本机管理入口回归；现有来源/Origin/真实 TCP 校验不能因企业登录而省略。
