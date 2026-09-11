# desktop 模块约束

> 在途演进：[v0.25.0 功能设计](../versions/v0.25.0/功能设计.md)与
> [技术方案](../versions/v0.25.0/技术方案.md)均已确认并定稿。M1 Web 登录与设置、M2 入口和远端连接已实现；
> M3 Host 文件兼容已获确认；M4 已迁出 PTY、接入当前 Host WebSocket，本轮验证完成、待用户确认。文末说明覆盖连接与原生资源目标约束。

## 模块定位

`apps/desktop` 是 Tauri 2 桌面应用，负责 WebView UI 与 macOS/桌面系统能力，并通过
`assistant-protocol` 和 Runtime Client 把 UI 意图发送给独立 Runtime Host。

修改本模块前必须同时阅读：

- 前端：[`docs/specs/前端编程规范.md`](../specs/前端编程规范.md)
- Rust：[`docs/specs/Rust编程规范.md`](../specs/Rust编程规范.md)

## 技术栈

- WebView：React、MobX、Sass、TypeScript、Vite、`@tauri-apps/api`。
- 原生层：Tauri 2、Rust。
- macOS 渲染：系统 `WKWebView`。
- 当前前端架构决策见 [`v0.14.0 技术方案`](../versions/v0.14.0/技术方案.md)；更换视图框架、
  状态方案或样式体系时必须先更新技术决策。

## 职责

- 主窗口、未来的悬浮球、托盘、快捷键和窗口生命周期。
- 文件/目录选择、通知、屏幕捕获、TTS 和系统权限申请。
- Tauri command 参数适配、Runtime Client 调用、Runtime 事件转发和错误展示转换。
- 应用启动、隐藏、退出以及 Runtime 的连接、启动请求和受控停止入口。

## 核心约束

- Desktop 独立构建与运行不得依赖 Client 工程、交互库、构建脚本、可执行入口或随包 Node。
  共用的协议类型和生成器必须独立于 Client；Desktop 自行发现、认证和管理 Host，通过
  discovery／systemd 读取真实来源，不解析 Client 私有安装布局或调用其生命周期代理。
  Client 未来替换架构不要求 Desktop 同步改造；该边界见 [v0.25.2 技术方案](../versions/v0.25.2/技术方案.md)。
- `src-tauri` 依赖 `assistant-protocol` 和 Runtime Client，不直接依赖或装配
  `assistant-runtime`；Tauri command 必须薄，不实现会话或 Agent Loop。
- Tauri Rust 启动层从 Runtime Home 私有发现文件取得 loopback 地址和进程级
  Token，将它们作为启动配置注入受信任 WebView。WebView 内的 HTTP Runtime Client 可
  直接调用 Runtime Command、Upload 和事件流，不要求每个请求都经 Tauri command 代理。
- 远程连接是默认关闭的可选能力；v0.25.0 已确认支持 HTTP／HTTPS 地址和远程身份，
  HTTPS 可选。M2 已实现同一 WebView Runtime Client 复用命令、事件和上传契约的目标连接。
- 开发 Desktop 使用普通 `npm run tauri -- dev` 连接兼容的 Release Host。显式 Token 请求
  使用 `credentials: omit`，Host 不根据 Vite／Tauri 来源或构建模式改变认证与权限；
  Cookie 登录及 API 保留同源保护，普通凭据不能提升为本机管理权限。
- WebView 中的连接 Token 只存内存，不进入 `localStorage`、日志或普通前端事件；进程 bootstrap 不进入 URL。
  打开 Web 快捷入口仅使用独立普通登录 token fragment，页面立即移除后建立 Cookie 登录态；
  配套 CSP、受控导航和 Cookie 同源保护；显式 Token 采用统一 CORS。
- xterm DOM renderer 会动态生成字体、ANSI 色、光标样式表及 truecolor/对比度内联样式，因此主窗口
  CSP 的 `style-src` 允许 `'unsafe-inline'`，并仅对 `style-src` 关闭 Tauri 的自动 hash/nonce 注入，
  避免注入的 hash 使该许可失效；脚本 CSP 与其他指令继续保持限制。终端内容仍通过 xterm 写入，
  不转换为 HTML。终端渲染验收必须使用正式 CSP 的 Release WebView，检查等宽字、ANSI/truecolor、
  光标和尺寸变化，不能以放宽策略的独立验证页替代安装包配置。
- 事件订阅使用能携带 Authorization header 的 `fetch` streaming SSE，不在 URL 中传 Token。
- 浏览器可使用随包 Web 的同源 Cookie，或显式提供普通 Token 调用 API；凭据与权限统一，
  不增加独立的 Origin 配置或 companion bridge。
- WebView 不直接访问文件系统、模型 API、数据库或 Shell；Host 文件与终端通过受认证 HTTP／WebSocket，客户端 OS 能力通过受控 Tauri command。
- 关闭主窗口或退出桌面客户端不得默认终止 Runtime；只有明确执行“停止 Runtime”，或在当次退出
  确认中选择“同时停止 Runtime”，才触发受控关闭流程。“退出 Assistant”不作为独立动作。
- 桌面专属数据（窗口大小、悬浮球位置、快捷键）不进入 Agent 会话模型。
- Runtime 配置（模型、工具、调度）不应散落到前端 localStorage。
- capability 按实际需要最小开放；修改窗口标签时同步检查 capability 绑定。
- 自定义 command 需要明确请求/响应类型、错误映射和权限语义。
- 附件或工具文件不支持应用内预览时按正常能力回退展示友好说明，不直接暴露 Runtime 原始错误；
  “使用系统应用打开”和“在目录中打开”都只向窄 Tauri command 提交稳定资源身份，由原生层
  回查 Runtime 后执行，WebView 不得提交任意本地路径。
- 子任务树和子 Agent 消息复用主 Conversation 的 Message、Reasoning、Tool Detail 与分页组件；
  进入子任务只替换中栏消息 owner，不创建子 Session、子 Composer 或第二个右侧上下文 owner。
- 子 Agent 审批继续消费父 Session 的唯一 pending approval 投影，并使用同一底部审批工作区；
  主视图和子视图不得同时复制一份阻塞列表。子任务自身 Usage 放在任务树和子视图，UI 不聚合父子
  Usage。
- 正式 Desktop 不依赖已退役的私有 Web Demo、整份 Conversation 命令或 Demo capability。
- WebView 生产源码与测试代码物理分离：`src/` 不放测试文件；单元/组件测试、测试支持代码和
  Playwright 场景分别集中在 `tests/unit/`、`tests/support/` 和 `tests/e2e/`。

## v0.17.0 Tool Image 详情边界

- 工具详情预览请求必须携带完整 `ConversationOwner`；child task ID 进入独立 URL 命名空间，不能
  只用 Session/message ID 猜测归属。
- `SessionToolImage` 只在对应工具详情中显示有界图片预览。它不进入附件 Store、Context Panel、
  通用产物列表或导出，也不展示系统打开、目录揭示和复制物理路径操作。
- `read_image` 详情以唯一的 `SessionToolImage` 原图为主体，打开弹窗后直接加载；来源路径、媒体类型
  和大小只作为紧凑附属信息，不再要求用户依次展开请求、结果、文件列表后手动触发预览，也不使用
  Host 请求时实时生成的 320px 缩略图。
- Host 返回缺失或损坏错误后，当前详情把对应项标记为不可用；关闭或切换详情时清除此 UI 派生态，
  不修改可靠 Conversation。
- 多路径文件审批必须逐条展示 `ToolApprovalSubject::Files.paths`，不能只显示工具名或第一条路径；
  Runtime 提供的 Session/Workspace 持久批准会精确保存每条路径，Desktop 不自行折叠授权范围。

## v0.18.0 M3 Goal 提交兼容边界

- 现有 Composer 显式以 `mode: normal` 提交输入，确保协议增加 Goal mode 后普通输入行为不变；
  TypeScript 类型继续由 `assistant-protocol` 生成，前端不得复制字符串枚举。
- M3 不在 Desktop 暴露 `/goal`、Goal 标签、状态或控制入口。客户端必须等 M5 的正式快照/命令
  闭环后再于 M6 实现交互，不能根据输入文本、`update_goal` 工具结果或 Accepted Run 猜测 Goal。

## v0.18.0 M5 Goal/WorkPlan Client 边界

- Runtime Client 与 RootStore 已接入协议生成的 SubmitInputMode 以及 ClearWorkPlan、Stop/Resume/Clear
  Goal 命令；调用成功后统一重新读取 SessionView，不在前端预测 generation、状态或后继 Run。
- `work_plan_changed` 与 `goal_changed` 进入既有 Session invalidation/refresh 调用链；sequence gap、
  reconnect 和未知事件仍以全量快照恢复为准。
- M5 只打通数据和意图，不增加 `/goal` 草稿解析、Goal 标签、Todo/Goal 展示或 Composer 布局。
  这些 UI 行为必须等 M5 确认后在 M6 依据交互指导实现。

## v0.18.0 M6 Composer 交互边界

- `/goal` 只武装下一次完整提交；标签属于 Composer 草稿状态，只能点击 `×` 取消。提交失败必须保留
  正文、附件与标签，成功后才清空；已有 Goal 和 capability 不支持时必须使用 SessionView 禁用。
- Todo 与 Goal 是正交投影：Todo 浮动摘要不占 Composer 正常布局高度；Goal row 占正常高度且只在
  点击后像 Queue 一样向下内联展开有最大高度的详情，不使用浮层，也不因 hover 或单纯获得焦点自动展开。
- Todo 详情以摘要条中线居中，采用紧凑宽高上限；位置必须由 trigger/overlay 实测 DOM 尺寸计算并
  保留视口夹取，不能写死相对偏移。详情是 `pointer-events: none` 的纯检视层，不提供清空计划入口，
  鼠标离开摘要后立即关闭；objective 标题允许换行并驱动 header 自动增高。摘要使用浅色 primary
  surface；loading 只在 Session 存在活动 Run 时显示，空闲时保留摘要但隐藏动画。详情使用实色 surface
  和中等阴影，并在会话区背层生成与详情高度联动、向上渐隐的 message list 底部模糊遮罩，不创建
  全屏蒙层或第二份 JavaScript 高度状态。
- Goal 与 Queue 复用 Composer 私有二级抽屉外壳和无局部 hover 的 header；Goal 详情不重复
  objective header、generation 或操作 footer，Paused 的退出收敛为摘要行 `×`，二次确认后才发送
  ClearGoal。收起和展开态都必须用相同负 margin/padding 覆盖下一区域圆角；展开态再增加外壳和正文
  底部留白。两类抽屉使用同一算法和单一展开状态，同时最多展开一个，不新增悬浮层。
- 纵向顺序固定为 Todo floating、Goal row、Approval Restore、Queue、Approval/Input。完整 Approval
  只与输入区互斥；Approval Restore 和 Queue 可以同时出现，不能为了压缩高度互相替换。
- 底栏常驻视觉项固定为添加、执行设置、上下文用量、模型设置、发送/停止。执行 variant 与 approval、
  model 与 reasoning effort 各自独立，只能以分类级联组织，不得在客户端构造排列组合状态。
- Runtime Goal 的 Running/Paused、budget、pause reason 和 held Queue 都以 SessionView 为准；Goal
  完成、空 item WorkPlan 和全完成 WorkPlan 均由 Runtime 自动清除，Desktop 只响应快照移除对应 UI，
  不自行判断完成。对旧版本已经投影的空 item WorkPlan，Desktop 仅隐藏 Todo UI 作为兼容兜底。
  Desktop 只调用 Stop/Resume/Clear/ClearPlan，不预测 generation 或 continuation。
- image handling 只在 Context Panel 作为模型能力说明展示，不占 Composer 底栏；标准宽度与 `720px`
  窄宽度必须保持主操作、Ask/Auto 状态可辨认且无主区域水平滚动。
- 工作空间行的移除只发送 `remove_workspace` 假删意图；执行前必须二次确认并明示不删除
  本地目录或历史会话。已移除 Workspace 及其 Session 不得进入会话边栏、标题搜索或
  新会话候选，也不得被归为“独立会话”。重新添加同一目录时，恢复原 Workspace ID
  和关联 Session 的展示。

## v0.21.0 M4 设备管理与托管边界

- `DeviceGatewayStore` 只保存当前 Host 快照、加载/失效和交互反馈；启停接入、配对、重命名、撤销
  和 PC 输出托管都通过正式 Command 表达，客户端不把这些动作转换成 Agent 输入。
- Gateway SSE 事件只是快照失效通知。连接建立、重连、stream gap 和事件合并刷新都重新读取 Host
  组合快照；Desktop 不用本地时钟计算配对窗口或候选过期，也不根据最近事件猜测设备在线。
- 设置中的“智能终端”承载完整设备管理；一级设备列表只保留名称、在线状态灯与管理动作，页面卡片
  标题不附加重复说明，连接能力、输出偏好和身份时间下沉到统一返回模式的二级详情页。Composer
  内的紧凑频道图标是托管状态查看、更换和解除的唯一入口；菜单只投影在线目标，用前缀图标区分
  Desktop、智能终端及未来其他渠道，不显示副标题或在线文案。Header 与右侧信息栏不再重复展示。
  顶部普通空闲/运行 tag 已移除，不能用长文本托管提示重新占满标题栏。
- Controller Conversation 始终保留所有显式输入与最终输出。Device UserMessage 在正文下方只显示
  可点击的设备名来源标签，输入模态、冻结回复偏好和设备 ID 进入来源详情弹窗；附件仍固定在正文
  上方。PC 输出托管只增加附加投递，不把 Desktop 与 Device 改成互斥输出渠道。
- M4 只读展示 Host 的 ASR/TTS `ready | degraded | unavailable`，不增加 Provider、模型、声音和密钥
  编辑表单；真实语音输入、播放及系统权限验证必须等待对应语音里程碑。

## v0.22.0 M1 Workspace 编辑交互边界

- Workspace 左栏、新会话菜单和右侧上下文统一使用 label；新建与编辑共用一个产品弹窗，同时编辑
  名称、主目录及有序附加目录，不在全局设置中增加 Workspace 管理页。
- 左栏 Workspace 菜单与右侧 Workspace Section 的编辑按钮只打开同一编辑状态。右侧名称取当前
  Workspace，目录取 Session 创建时冻结快照；目录变化时明确提示既有会话仍使用旧目录，不能据此
  修改 Session System Prompt。
- 右侧目录的打开动作以 Session 与冻结目录序号请求受信任原生桥接，不能仅按当前 Workspace ID
  重新查主目录；复制动作只复制已投影路径。脏表单关闭使用产品 Dialog，不调用浏览器确认框。

## v0.22.0 M5 Composer 图片粘贴与 selection 生命周期

- Composer 只检查本次 `paste` 事件中的 `image/*` 文件项；纯文本粘贴保持浏览器原生行为，图文混合
  粘贴在当前选区插入文字并按剪贴板顺序暂存图片。Web `File`、Base64 与 Object URL 不进入草稿事实。
- 剪贴板图片通过原始字节 Tauri command 写入匿名临时文件，并与文件选择器结果统一为原生 selection。
  原生层按 magic bytes 识别真实图片类型，限制单文件 32 MiB、单 Composer 32 项、进程内临时图片
  256 MiB；任何一项失败都释放本批已创建 selection，不留下半批 Tag。
- 附件 Tag 正文打开统一详情弹窗，移除按钮保持独立命中区。selection 上传前由受控句柄预览，上传后
  可回退到 Runtime 附件预览；界面不得展示或复制物理路径。
- selection 随对应 Workspace/unbound 草稿保存于当前进程，切换草稿不互相混合。显式移除、草稿清空、
  Workspace 移除、可靠发送成功、TTL 回收或应用退出都会释放临时资源；失败重试继续复用仍有效 selection。
- 普通 Session 与首次物化都允许“附件非空、文本为空”的提交。发送按钮布局继续使用自适应宽度和
  文本控件最大宽度，不能因草稿态缺少可选控件而把右侧模型/发送操作挤出 Composer。

## v0.22.0 M6 会话文本引用交互边界

- Conversation 以可靠消息正文根为边界，Selection 两端在同一根即可跨文本节点、行内代码、链接和
  强调；跨消息、工具块、交互控件和流式未可靠正文直接拒绝。浮动气泡执行纯前端冻结动作，不发送
  Runtime command；右键和 `Shift+F10` 保持 WebView 原生行为。
- 消息结构的 `user-select: none` 只用于防止误拖选，不能依赖它清除已有选区。鼠标左键点击空白、
  控件或会话外部时清除属于当前消息列表的选区；保留右键复制、Shift 扩选与引用按钮操作，
  选区折叠后同步隐藏引用气泡。不得通过默认选中或重建 Range 来维持引用入口。
- `QuoteTextProjection` 用 TreeWalker 连接可引用文本节点，生成 UTF-16 range、exact 与 Unicode 有界
  prefix/suffix；prefix/suffix 每侧最多 128 个 Unicode 字符，链接、代码和强调等渲染结构统一冻结为
  可见纯文本，不反向转换或匹配 raw Markdown。
- `ComposerQuoteStore` 仅在 WebView 生命周期内按主 Session 保存最多 16 条引用；子任务选区仍加入其
  父 Session Composer。引用与附件在同一 Context Item 区按用户加入顺序展示，点击 Tag 打开统一详情；
  失败保留，可靠发送成功才移除。内部 locator 不进入产品文案。
- 来源定位直接复用持久 locator 和普通 Conversation 分页；`TransientFocusStore` 只持有一个瞬时 range。
  优先用 CSS Highlight 非侵入高亮，不支持时退化为可回收描边；新定位、发送同步起点、滚动、会话或
  子任务切换、定位失败、超时、组件卸载都必须清理，发送后不得残留。

## 不应放在本模块的内容

- Agent Loop、模型 Provider 和工具选择策略。
- 多会话调度、Run 状态机、定时任务与持久化业务。
- 为 UI 方便而复制一套协议类型并产生不同语义。
- 通过前端直接执行 Shell 或读取任意本地文件。

## Harness 验证

```bash
cd apps/desktop
npm run build
npm run tauri -- build --no-bundle
```

涉及窗口、托盘、快捷键、屏幕捕获、TTS 或系统权限时，必须补充 macOS 实机验证并说明权限状态。


## v0.24.0 资源栏与原生资源生命周期

- 右栏由 Desktop 按 Session/草稿持有标签组；会话切换选择所属组，迟到的文件、浏览器回调必须保留原 owner。轻量索引、查看位置与页面实例分离；非终端页面共用 20 实例 LRU，终端不参加淘汰。
- 持久快照只复用桌面偏好 JSON，不进入 Runtime 数据库。只保存可校验的资源描述、最后 URL、终端启动来源和有界查看状态，不保存文件正文、网页 DOM、凭据或终端输出。恢复完成前不能以空状态覆盖磁盘；受控退出先 flush，后回收终端。
- 跨客户端重启重新登记本地文件句柄、加载最后 URL；终端标签首次激活才按原启动来源创建新 Shell。它与同一客户端内保留 PTY/xterm 的会话切回不同，不宣称恢复旧进程或运行中命令。
- 本地文件链接只在受信任 Assistant 消息正文启用，使用显式绝对 file URI，经原生校验和有界读取后返回受控句柄。System Context、Skill、普通路径和不受信任外部网页不获得该能力。
- 文件类型图标与映射直接引用 Material Icon Theme；Monaco 仅按需加载只读编辑器及所需语言，Markdown 复用消息渲染器；图片缩放/拖拽直接引用 react-zoom-pan-pinch，变换保存到现有 ResourceViewState 和偏好快照。图片与 PDF 使用受控 Blob URL 并随页面释放。
- Monaco 的 `editor.api` 按需入口必须显式加载官方 `features/codicon/register`，不依赖 JSON 等语言功能间接加载图标字体；首次打开非 JSON 文件时也需保证查找栏图标正常。
- 外部网页是同窗口 child WebView，不共享主 WebView 的 bootstrap、command 权限或 DOM。资源 `+` 采用原生菜单；其他 HTML 遮罩按已确认方案暂时隐藏网页，不靠 CSS z-index 声称覆盖原生层。
- 浮层原语用 `data-overlay-region` 声明实际绘制范围；浏览器只读取 Shell 的 overlay root 并做视口矩形相交判断。Dialog 的遮罩覆盖窗口，非相交菜单不隐藏网页；Tooltip 的 `pointer-events:none` 不影响视觉遮挡判断。HTML 浮层始终独立渲染，不再操作整个 portal 的 visibility；内部滚动只测量，原生显隐只随遮挡结果变化，窗口缩放／恢复另行重应用布局。
- macOS 可见原生网页最多每秒更新一张内存截图，遮挡时停止采样并露出占位图，解除后恢复同一个 WebView；截图不入偏好或磁盘。导航、切换和关闭清理快照并拒绝迟到结果。截图失败或非 macOS 返回不可用，显示中性占位，不能使正常网页进入错误态。
- 用户已批准仅 `browser_resource/platform.rs` 的 WKWebView FFI 局部允许 unsafe。仅主 WebView 可调用受管网页截图；复用 Wry 的 objc2/block2 版本，主线程调用，长边限制、2 MiB 编码上限与单任务门控；不引入屏幕录制权限、网页脚本截图或调试监听端口。
- 截图校验和地址轮询共用空值安全的原生 URL 读取，避免 Wry 0.55.1 对未提交／失败页面的 nil URL 直接 unwrap。私有 bridge 返回 null 时保留请求地址；加载事件使用 payload 的地址。
- macOS 的 `tauri-runtime-wry` 精确固定为 2.11.4。该版本 `WithWebview` 将网页、controller、window 的三个 +1 引用转成裸指针却不释放，`platform.rs` 统一用 RAII 接管并平衡，防止截图／URL 轮询后关闭网页仍播放音频。升级到包含上游 [#15224](https://github.com/tauri-apps/tauri/pull/15224) 的借用实现时，必须同时移除接管逻辑，不能直接升级依赖。
- 真正释放网页（关闭标签、LRU 回收、主页面重载）统一经过原生清理：停止加载，macOS 12+ 禁止该页媒体继续／重新播放，卸载文档，再关闭 Tauri 子视图；较旧系统不调用不可用的媒体 API。普通切换和浮层遮挡只隐藏，不能触发此清理或影响其他网页。回归方法见 `apps/desktop/tests/native/README.md`。
- 原生窗口操作使用 `get_window("main")`。创建 child WebView 后不得再把 `get_webview_window("main")` 当作窗口存在的依据。隐藏窗口保留 PTY；真正退出等待进程组清理，不能绑定或停止独立 Runtime 的后台任务。
- 用户终端与 Agent Shell 完全分开，环境变量过滤私有凭据，后台输出受 ack 背压及 xterm scrollback 上限约束。Ctrl+D 只发送 EOF，收到实际 Shell 退出事件才关闭标签；Session 删除先完成所属终端回收，再提交删除意图。

## 开发 Runtime 目录与验收隔离

- 2026-09-07 用户指定开发环境改用 `~/.ez-assistant`：`npm run tauri -- dev` 在未设置 `EZ_ASSISTANT_RUNTIME_HOME` 时注入该绝对路径，构建子进程与 Desktop 继承同一环境。显式覆盖仍优先；空值或相对路径由原生校验拒绝，不静默回退。手动开发 GUI／Host 也显式传入该目录。
- 自动化与隔离验收继续显式使用临时目录。未经过启动脚本且未指定环境的 debug／Dev 二进制仍使用 `~/.ez-assistant-dev`，正式产品 Release 默认目录不变。
- `npm run tauri -- dev` 合并 `tauri.dev.conf.json`，应用标识为 `com.ez-assistant.desktop.dev`，隔离 Desktop 偏好、窗口状态和 WebView 存储；验证 Release 时同样合并该配置。开发配置不重复定义 `app.windows` 数组，避免覆盖 macOS Overlay／隐藏标题或其他平台的窗口配置。
- 本轮 M0—M3 使用两个独立 Runtime Home / Host 进程联调，不启动或连接用户已安装应用的 Runtime。
- 人工开发／验收保留正常操作系统 `HOME`，确保用户终端能加载既有 Shell 配置及主题；不要复用自动化夹具的空用户目录作为人工验收环境。Runtime Home 按当前用户指定目录或显式隔离覆盖选择。


## v0.25.0 M1/M2：Web 登录与 Desktop 目标连接

- `ApplicationConnectionStore` 管理客户端入口和登录展示；每次实际切换释放旧 RootStore，创建新目标的业务投影，不复制 Runtime 权威状态。入口静默启动本机，用户手动进入；进入后仅从设置切换，失败不回到入口、不回退本机。
- 设置侧栏统一为 Runtime，概览与切换、访问设置、状态与诊断、本机与客户端共用 `SettingsStore.page` 导航及未保存确认，不另建路由状态。访问与诊断指向当前 Host，本机页只承载本机原生操作；Web 隐藏切换和本机页，Desktop 切换成功回新目标概览。
- 原生 `runtime_connection` 持有当前选中目标与不透明 binding ID；`RuntimeTarget` 在 Tauri 调用准入时冻结上下文。Host 字节请求、上传、临时文件与迟到返回均绑定同一目标；切换取消旧操作并释放原生资源。已有业务 HTTP／SSE 继续复用连接协调和取消机制。
- 本机 Runtime 启停、重启和托盘属于本机进程管理，不能修改当前远端连接或把其会话计数当成本机影响范围；远端凭据失效后保留远端选择与设置错误状态。
- 远端地址只接受 HTTP／HTTPS origin，拒绝用户信息、路径、查询和 fragment；Native 请求禁用重定向并使用正常 TLS 校验，不提供忽略证书错误选项。认证与协议／组件版本验证通过后才进入业务页面。
- macOS 可选记住密码写入 Keychain，service 随应用标识隔离、account 使用规范化 origin；成功登录后才保存。Keychain 不可用仍可连接并提示，不写入明文偏好，不自动选择上次目标。
- 顶栏“打开 Web 端”取得当前 Host 的独立普通登录 token，经系统浏览器打开 fragment 链接；Web 移除 fragment 后使用 Host Cookie。浏览器退出不撤销 Desktop 自己的登录。
- DesktopEntryPage 与 WebLoginPage 通过 `RuntimeEntryLayout` 共用背景、源 Logo、标题、页脚及内容宽度；不同表单经 `children` 组合，共享层不接管连接或登录状态。DOTS 归属于共享布局，登录／连接进入工作台后统一卸载，Web 不渲染独立阴影卡片。
- DOTS 及 Three 从固定 npm 依赖本地打包、按入口懒加载；规则稀疏网格、按时间计算的小幅波动与鼠标微动；固定高度基准避免按帧累加导致低帧率下振幅变小，点与线共用位置缓冲。离开入口销毁动画、WebGL 场景与监听；隐藏页面暂停，减少动态效果或初始化失败时显示静态背景。
- Web 只加载可用原生能力。Host 目录选择、浏览器文件兼容和按目标保存查看状态已由文末 M3 实现覆盖；Host 文件路径不能交给客户端 OS 打开。Host PTY 现由文末 M4 实现覆盖。


## v0.25.0 M2：统一 Host 端口

- 访问设置只展示一个服务端口（默认 7240）、可选域名与所选协议；本机 discovery 支持同一监听的
  HTTP／HTTPS，仍只接受私有发现文件中的 127.0.0.1 地址，不放宽原生证书校验或将 bootstrap 发往域名。
- 访问开关／域名立即生效；端口／协议／证书修改后显示待重启，仅本机 Desktop 可复用现有重启确认。


## v0.25.0 M3：文件兼容与查看状态

- 每个 RootStore 拥有一个 `ClientResources`，共用当前 Runtime Client 的认证和取消域；Desktop 保留原生选择及流式上传，Web 用浏览器 File／FormData。选择、在途操作与下载 Blob URL 随 owner 释放，迟到结果不得进入新目标。
- Web 文件选择最多每次 32 项、总暂存 128 项、单文件 1 GiB；粘贴图片沿用现有大小校验。首次发送复用 manifest 与 materialization key，网络中断返回既有“不确定结果”语义，不能通过创建新会话重试。
- 本机 Desktop 工作目录继续使用系统选择器；Web／远端 Desktop 使用同一个 HostDirectoryDialog，支持任意 Host 目录、上级、隐藏项、手输路径及明确错误。WorkspaceEditor 复用该选择能力添加或更换目录。
- Assistant 绝对路径／file URI 和相对引用解释为 Host 文件；独立路径使用 `host_file` 来源，不复用客户端 `local_file`。Session locator、附件和工具资源身份沿用既有协议。
- Web 下载认证 fetch 的 Blob 后交给浏览器保存，URL 由当前 owner 有界释放；Desktop 通过已有原生保存流程流式写入临时文件，当前目标变更时取消，成功后发布目标文件。原生系统打开、Finder 及技能源目录入口仅在本机 Desktop 提供。
- 同一原生偏好文件保留本机顶层字段，远端按规范化 HTTP／HTTPS origin 放入有界 `hosts` 映射；写入串行合并，不能覆盖本机或其他目标。Web 继续使用当前 origin 的轻量查看存储；不保存正文、File／Blob、密码或 token，存储不可用时降级。Web 不恢复 browser／terminal，远端不恢复 `local_file`。
- 复制功能共用 Clipboard API 与普通 HTTP 下的用户触发复制回退；失败给出可手动复制的内容。M3 不启用 Web 内嵌浏览器；远端终端现由文末 M4 实现覆盖。


## v0.25.0 M4：Host 终端客户端

- `runtime-client/TerminalSocket` 由当前 RuntimeClient 绑定 origin、凭据与取消域；Web 沿用同源 Cookie，Desktop 首帧 Bearer。凭据不进入 query/subprotocol，不自动重连终端。
- `TerminalController` 继续持有 xterm 展示和标签状态，写入按 Host `input_ack` 保序，输出在 xterm 解析后 ACK。重开创建新 socket；Web pagehide 直接断开所属连接，查看偏好不恢复 Web 终端。
- 删除 Tauri user_terminal manager、PTY 和进程组清理依赖；正常前端退出等待连接清理，原生退出销毁 WebView 后由 Host 回收断开的 socket。隐藏窗口不会关闭 socket。
- CSP 允许当前 Host 的 WS／WSS，HTTP 与 HTTPS 地址推导一致；切换或 RuntimeClient dispose 只关闭自身连接，不发送清理全 Host 终端的命令。

## v0.25.0 M5：首次工作台转场

- `WorkspaceTransition` 只组合入口和实际 `AppShell`。Desktop 首次连接或 Web 登录及初始快照就绪后才开始播放，不新增业务连接状态；Desktop 设置切换 Host 不重播。Web 密码、token 快捷登录和 Cookie 恢复共用转场，退出或登录失效时销毁本轮视图，重新登录重播。
- 火箭使用用户委托动画任务的透明图集，播放器懒加载且仅保留当前／下一张解码图，完成、卸载或失败时释放图像、请求、rAF 和监听。素材说明见 `apps/desktop/src/features/runtime-access/WorkspaceTransition/README.md`；视觉认可状态记录在版本计划。
- 转场期间工作台 inert，入口上移淡出；结束后聚焦可用输入框，否则聚焦工作台容器。减少动态效果、后台切换或媒体失败直接进入已就绪的工作台。
- Desktop CSP 的 `connect-src 'self'` 用于读取包内协议图集，不能仅凭 HTTP Vite 测试判断原生资源可加载；xterm 所需样式策略保持。Vanta／Three 许可证随 `licenses/entry-visual.txt` 打包。
- Web 登录沿用连接代次，在登录、bootstrap、退出的异步边界丢弃旧结果；迟到响应不能恢复已经退出的 UI 投影。

### v0.25.0 当前 Skill/MCP 列表

`/skill` 每次呼出、新草稿选择与右侧上下文技能栏复用 `ListSkills`，不读取 Session 目录字段。右栏进入／展开、技能设置或刷新事件、窗口重新激活时查询；列表只有视图生命周期的加载／错误状态，关闭或换 owner 丢弃旧响应。当前列表不补回已删除的历史激活名称。
`/mcp` 每次呼出查询 Runtime 当前服务 Registry；选择标签和已冻结上下文不作为可选目录来源。系统提示词保持冻结。


开发构建复用本机 Host 时，在 bootstrap 返回就绪前使用 Vite 的精确 Origin 执行预检；原生
健康检查成功不等于 WebView 具备 CORS 访问能力。显式 Token 的预检接受精确来源或通配符；
旧 Host 拒绝时提示更新，不自动停止既有进程；原生停止／重启通道不受此预检限制。

## v0.25.1 M2 启动等待

原生 bootstrap 以认证 capabilities 确认已存在 Host，可达不等于业务就绪；初始化中不得反复
launch 或因 readiness 超时杀死 Host。页面在创建 SSE、快照及资源恢复前等待 health Ready；
失败停止自动重连并显示修复指引，连接释放只取消客户端等待，不取消 Host 的迁移或恢复。
Desktop 与 Web 使用同一启动状态解释；不提供旧业务形状回退。2026-09-10 用户明确 v0.25.2
接入按应用协议最低兼容版本双向检查，不兼容时提示启动／连接失败；该方向替代“必须同版本”
的判断，具体接入方式与现有协议检查的职责统一由技术方案明确，尚未实现。

## v0.25.1 M3 模型管理客户端

- ApplicationSnapshot 的 providers/model_settings 和 Session 的 nullable model_selection 是新投影；
  默认引用不进入全局 ConfigurationStatus。新会话草稿的空引用始终表示跟随当时默认，不在打开草稿时冻结默认。
- 设置按“模型与服务商 → 服务商 → 固定参数”组织。在线目录只属于正在展开的页面／服务商，
  刷新、失败、离开后清空；固定状态在模型行内展示，不设独立固定列表或分页界面。未返回的固定项明确标注离线／本次未返回，与本次在线候选合并去重并保留来源标记。重置成功与随后的在线读取失败分别展示。
- Settings 的 ModelPicker 通过 feature 入口供 Composer 复用；SettingsCascadePopover 与 AnchoredOverlay
  上提公共组件，保留键盘、定位和 Portal 行为，不反向依赖 feature。模型列表按展开服务商读取，推理强度是独立项。
- 固定参数表单保留未知值，在线参考刷新不覆盖草稿；用户采用后仍需保存。保存响应返回的时间戳／参数为权威结果；
  保存结果不确定时只读确认，不自动重发写命令。已提交保存后的应用刷新失败必须明确提示“保存已完成”。
- 设置读取核对 Runtime Client 身份；新读取／成功 mutation 使先前查询失效，避免迟到的列表覆盖已保存连接。
  此查询标识只存在于 WebView，不是数据库或应用协议版本。

### v0.25.1 M3 接入复核修正

- 服务商表单按类型预填发现格式与同源列表路径，移除列表格式选择；百炼 API／套餐分别使用
  dashscope_native／openai，Kimi 使用 moonshot。标准操作复用 Button，厂商文档使用普通链接。
- 参数详情使用 Runtime 返回的字段来源与有核查日期的模板参考；固定记录整份显示，不以模板刷新覆盖。
  思考强度是五个标准档位到字符串线上值的映射，空置即不支持；默认值只选已填档位。
- 设置分组复用 CollapsibleSection（原生语义标题按钮与 Collapse）；级联触发器的内联布局归 SettingsCascadePopover。
  双面板放不下时由同一组件切换为单面板、显示返回并恢复一级焦点，子操作不再被菜单选项样式覆盖。

模型快速选择二级不重复服务商标题，不显示刷新按钮或成功态的空内容区；所属服务商通过一级活动项与菜单可访问名称表达。
重新进入服务商即重新获取列表；窄屏仍保留返回一级，失败保留重试。其他设置类别的标题保持原有显示规则。

SettingsCascadePopover 支持可选清除动作，由触发器尾部统一封装。默认／辅助模型选择启用，
有值时 hover/focus 切换箭头为关闭图标；清除按钮与展开按钮为同级原生按钮，不嵌套交互元素。
空值无清除动作，禁用／提交中不可重复触发；用户引用清空仍由 Runtime command 返回结果决定。

会话模型选择器将“默认模型”作为首个一级可选项，使用选中标记表示跟随全局默认，直接选择不展开子菜单。
推理强度分类前由 SettingsCascadePopover 的 separator_before 渲染分割线；一级可选项、服务商和页脚共用键盘导航。

模型参数详情表单不展示顶部来源提示和字段来源副标题；标签仅显示字段名，在线／模板／固定参数的预填与编辑规则保持不变。

CollapsibleSection 默认无外部 margin，布局间距由父容器负责；标题为自然文本行（最小 24px），
无按钮边框、底色或水平 padding，箭头左对齐；只有展开内容带 12px 顶部间距，收起不占内容空间。
保留键盘操作、可见焦点及展开状态语义。

ModelFieldSelector 在封装内应用共享 form_field 标签样式，标签与输入字段字体和间距一致；
默认思考强度放在档位的同一个 model_form 网格中，桌面占一列，窄屏跟随统一单列布局。

模型参数页的厂商文档链接与核查说明在同一行（窄屏自然换行）；桌面点击复用 openExternalHttpUrl
调用系统默认浏览器，失败局部提示，Web 保持标准新标签页链接行为。

用户界面的 Endpoint 统一称为“服务地址”，涵盖表单标签、连接摘要及错误提示；协议与持久化字段仍使用 endpoint。

通用 Button 的 danger hover 显式保留危险色背景与白字，只做轻微亮度变化，避免被普通按钮 hover 灰底覆盖。

服务商 API Key 在输入框右侧复用 InlineIconButton 作为眼睛显示开关，仅切换本次输入；清除和保存后恢复隐藏。
文本 input 统一有非空占位提示，无具体文案使用“请输入”，可编辑 SelectionPopover 在组件内部提供缺省值。

MarkdownContent 的有序列表保留原生编号，依据起始值与直接列表项数量计算最大序号位数并预留缩进，
不与无序列表共用固定 1.5em；多位编号、嵌套和流式增长分别按当前列表计算，换行对齐正文。

模型行用紧凑 Tag 展示创建来源和参数状态，来源取固定记录 origin，模板／待补全状态取 Runtime 的目录摘要。手动新增复用第三级参数编辑页；manual 删除模型、online 重置配置。选择器分别加载本次在线列表与固定记录，目录慢或失败时已保存模型仍可选择，关闭后丢弃本次视图结果。

开发便捷脚本 `apps/desktop/start.sh` 与 `host-restart.sh` 接受一个位置参数指定 Runtime Home；优先级为入参 > `EZ_ASSISTANT_RUNTIME_HOME` > `~/.ez-assistant`。相对路径按调用位置解析，运行命令前切换到 desktop 目录。重启脚本先执行 `npm run build:host`，构建失败保留现有进程；后续 Cargo 启动也继承 `EZ_ASSISTANT_WEB_DIST`，避免重新生成不含 Web 的开发 Host。构建成功后只读取目标目录发现文件，核对进程命令和目录后发送 SIGINT，最多等待 30 秒；核对失败或超时不启动新 Host，不全局 pkill。

## v0.25.2 M2 软件版本与共享协议消费

- DTO、发布常量、纯判定统一导入 @ez-assistant/protocol；不再引用应用私有 generated 目录。
- 原生发现与远端连接使用软件版本双向下限，缺少声明的旧 Host 明确拒绝，不替换现有实例。
  原生请求、前端 fetch、上传／下载均携带自身构建版本，不取 Host 版本冒充页面版本。
- Web 登录前声明页面版本；快捷登录交换后的普通会话使用当前页面声明。SSE 每次重建重新
  读取并校验 capabilities，再建立事件流；HTTP 409 和 PTY 兼容错误给出更新／刷新提示。
- 诊断显示 Host 软件版本及最低兼容软件版本。共享协议包、Desktop 构建和 Host 均不依赖
  Client 代码、CLI 框架或随包 Node；Client 尚未进入正式实现。

## v0.25.2 M3 本机生命周期（实装，本机验收通过）

- 受控停止固定已认证的地址、实例、token 和 PID；不重发现后向后继实例补发 shutdown。
  只有原进程退出且原实例锁可取得才完成，discovery 消失本身不证明已停止。
- 普通重启在停止前核对 Host 私有来源的可执行性、摘要及 build-info 版本对，停止后再核对，
  始终沿原来源启动；不可用时失败，不回退到 Desktop 随包版本或调用 Client 代理。
- 原生发现／请求遵循三秒请求、六十秒启动和三十秒停止上限，轮询 250 毫秒递增至一秒。
  原生 bootstrap 只证明安全连接；Starting／Unavailable／Ready 仍由既有 UI 生命周期消费 health。
- 来源验证属于 Desktop 的原生适配，不依赖 Client 安装、Node 或任何 Client 私有清单。

M3 已通过 macOS 四组真实 Host 隔离验收（后台与信号、双启动、原来源重启、来源缺失／变化拒绝），四库与八份备份逐表核验一致；不代表 Linux／SSH 或完整 Desktop GUI 已验收。

## v0.25.2 桌面启动与服务器自启边界

2026-09-10 用户明确：Desktop 与 Client 的自启逻辑不同，Client 主要面向服务器。Desktop 保留自己的桌面启动逻辑，本版不新增开机／登录自启功能，也不接入 Client 的 systemd 用户服务、linger 或服务器 unit 管理。两产品共存仍遵守共享 Host 的发现、实例锁、软件兼容和来源保护；不把共用 Host 推导为共用自启策略。
