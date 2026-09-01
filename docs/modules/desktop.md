# desktop 模块约束

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

- `src-tauri` 依赖 `assistant-protocol` 和 Runtime Client，不直接依赖或装配
  `assistant-runtime`；Tauri command 必须薄，不实现会话或 Agent Loop。
- Tauri Rust 启动层从 Runtime Home 私有发现文件取得 loopback 地址和进程级
  Token，将它们作为启动配置注入受信任 WebView。WebView 内的 HTTP Runtime Client 可
  直接调用 Runtime Command、Upload 和事件流，不要求每个请求都经 Tauri command 代理。
- 远程连接是默认关闭的可选能力；后续如显式启用，同一 WebView Runtime Client 改用
  HTTPS 地址和远程身份，不另造命令、事件或上传客户端。
- WebView 中的 Runtime Token 只存内存，不进入 URL、`localStorage`、日志或普通前端事件；
  配套 CSP、受控导航和精确 WebView Origin 白名单。
- 事件订阅使用能携带 Authorization header 的 `fetch` streaming SSE，不在 URL 中传 Token。
- 纯浏览器直连本地 Runtime 保留为未来显式开放的可选模式，默认关闭；它可通过
  独立的 Origin 白名单和授权边界直连，不强制引入 companion bridge。
- WebView 不直接访问文件系统、模型 API、数据库或 Shell；必须通过受控 Tauri command。
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
