# core-demo 模块约束

## 模块定位

`tools/core-demo` 是开发者显式启动的 v0.7.0 B/S 验证宿主，用于通过 `agent-sdk` 装配并验证
完整 Agent Core 能力。它不属于产品进程、正式 `assistant-runtime`、sidecar、daemon 或桌面
应用，其 Session、Run、Journal、Store、策略、HTTP DTO、CLI 和页面均为私有实现。

修改前必须阅读：

- [`Rust 编程规范`](../specs/Rust编程规范.md)
- [`前端编程规范`](../specs/前端编程规范.md)（修改静态页面时）
- [`Agent 系统技术架构`](agent-system.md)
- [`agent-sdk 模块约束`](agent-sdk.md)
- 涉及能力对应的 Agent crate 模块约束
- 当前版本功能设计、技术方案和开发计划

## 依赖与所有权

- 本工具是顶层开发目标，可向下依赖 SDK、Agent crate、Provider Adapter 和本地 Adapter；
  产品 crate 不得依赖本工具。
- `agent-testkit` 只能作为 dev-dependency，普通运行不能携带测试实现。
- 一个 Demo Session 持有一个按冻结 Prompt/ToolSet 构建的 Agent、私有 Journal 和活动 Run 闸。
- 同一 Session 的上下文变更 Run 串行；不同 Session 可异步并发并共享线程安全的底层服务，
  不为 Session 绑定操作系统线程。
- UI 只发送意图和展示投影，不持有 Session、Run、审批或 Agent Loop 的权威状态。
- Agent 事件消费与 SSE 广播必须解耦；浏览器断开、慢消费或 delta 缺口不能反压 Provider、
  取消 Run 或破坏可靠终态，最终状态以 snapshot/Journal 为准。

## 服务与安全边界

- 只绑定 loopback，端口默认由 OS 分配；不得提供非 loopback 监听配置。
- Demo 不实现访问令牌鉴权，但必须校验 Host、Origin 和 POST 来源，不开放通用 CORS，并设置
  CSP 与 `Cache-Control: no-store`。这些措施不构成本机进程身份认证。
- 页面中的模型文本、路径、command、工具参数、stdout/stderr 和错误必须按不可信文本渲染。
- Shell 以当前用户权限运行，工作目录和路径规则不能描述为系统沙盒。
- credential、完整 prompt、文件内容和 Shell 输出不得进入普通日志、错误、snapshot fixture
  或浏览器持久存储。
- 页面关闭不等于进程退出；managed/detached 子进程生命周期必须按工具契约明确展示和清理。

## 私有数据与能力装配

- `--workdir` 和 `--data-dir` 由开发者显式指定并在启动时冻结；测试只使用 tempfile。
- data-dir 不是文件或 Shell 工具的隔离边界；文件工具可接收绝对路径，Shell 以当前用户权限
  运行。人工验证必须使用专用目录，Store 不协调或防御工具及其他外部写入者。
- 私有 Journal 负责 begin/complete 和完成 Conversation，pending exchange 不进入规范快照。
- 私有 JSON Memory Store、RecallSource、Context continuation、Plan/Build、Ask/Auto、审批和
  Retry 只验证下层能力，不得提升为产品协议或公共 Adapter。
- 真实 Provider 只在显式 serve/人工验证入口读取 credential；`--help` 和自动测试保持离线。
- 不实现正式 Session 恢复、产品配置迁移、数据库、Trace Replay 或桌面集成。

## M0 边界

M0 只建立可编译工程、依赖和文档，命令仅支持无副作用的 `--help`。HTTP Server、Agent
Builder、Session、Journal、真实工具和 Provider 行为必须留给后续已授权里程碑。

## M4 基础宿主

- `serve` 必须显式接收 `--workdir` 与 `--data-dir`，端口默认由 OS 分配；M4 的 data-dir 只做
  后续能力预留，不保存 Session 或 Journal。
- 一个 Session 持有独立 Agent、内存 Journal 与活动 Run 闸；共享的确定性 ModelService 不含
  Session 状态。同一 Session 重入返回 `SessionBusy`，不同 Session 由独立 Tokio task 推进。
- M4 的离线确定性模型属于 Demo 私有实现，只生成 reasoning/text 流；不读取环境变量、真实
  Provider、Memory Store、文件或 Shell。
- Supervisor 持续消费 AgentEvent、更新短期投影，并通过有界 broadcast 发布带 sequence 的
  失效通知；Completion 和 Journal 才是权威终态。SSE lag 显式发送 gap，客户端重新读取
  global/session snapshot。
- 页面选中的 Session 只保存在页面内存，不写入 Server、localStorage 或 sessionStorage；刷新
  后通过快照恢复，不能取消仍在服务端运行的 Run。
- M4 已提供审批路由骨架，但没有 pending approval；正式 Plan/Build × Ask/Auto、安全策略与
  真实审批从后续已授权里程碑接入。

## M5 本地工具与安全闭环

- 每个 Session 复用冻结于显式 `--workdir` 的标准文件/Shell ToolSet，并拥有 data-dir 下独立的
 临时工作区、审批协调器和内存审计投影；Session 销毁时临时工作区随句柄清理。
- 离线确定性模型用 `/tool <name> <json>` 发起一次真实工具调用；`/repeat <count> ...` 只用于
 触发重复调用 Guardrail。它不把自然语言解释成命令，也不属于正式模型或协议契约。
- Plan 对每类 resolved 事实作出明确能力判断：workdir 只读、Session 临时工作区可读写、Shell
  以及策略无法识别的授权事实类型拒绝；M6 增加的 `list_pinned_memories`/`recall_memory` 作为
  显式只读名单允许。
  Build 先执行 Allow 优先于 Deny 的名单规则，未命中项再由 Ask 一次性审批或 Auto 的上层
  AllowAll 处理；Auto 不能越过前置明确 Deny。
- 文件授权显示 resolve 后绝对逻辑路径；Shell 授权显示完整 command、workdir、timeout 和
  managed/detached。同一 resolved invocation 同时用于授权与执行，不在审批后重新解析参数。
- 一次只允许一个 pending approval。决策、Run 取消和 future drop 通过一次性 channel 与同步
  Drop 清理收敛；重复或过期决策返回冲突，不产生第二次工具执行或第二个终态。
- 审计不保存文件内容或 Shell 输出，只保存 resolved 事实、策略、决策、执行状态、错误分类与
  exit code；实际文件结果和 Shell stdout/stderr 由权威 Journal/Run 投影展示。
- detached 调用是显式生命周期交接：Run 取消、Session 结束和页面关闭都不会停止已交接进程，
  验证者必须另行显式终止。所有真实工具测试和人工验证只使用专用临时 workdir。

## M6 Memory 与会话冻结

- Core Demo 在显式 data-dir 中私有维护 `pinned-memory.json` 和 `recall-records.json`；同目录临时
  文件完成 write、flush、sync 后原子替换。文件格式、revision 和 HTTP DTO 均不是公共契约，
  不新增 `agent-memory-local`。
- Pinned Store 的内存候选状态只有在文件替换成功后才提交；失败时旧文件和旧内存状态保持为
  权威事实。Demo 正常路径只由当前 Store 实例写入同一文件，但不把该约定描述为可抵御 Shell
  或其他外部写入者的文件隔离。
- 新建 Session 原子读取最新 Pinned entries/revision，渲染最终 XML Part，与基础说明和 Recall
  Source 说明一起构造 `SystemPromptSnapshot` 后再创建 Agent。Store 后续变化不更新当前 Agent。
- 恢复/重建语义只接受已经序列化保存的最终 `SystemPromptSnapshot`，不读取当前 Store 重新渲染；
  Core Demo 仍不承诺进程重启后的完整 Session/Journal 恢复。
- 标准 `pin_memory`、`update_pinned_memory`、`unpin_memory`、`list_pinned_memories` 与
  `recall_memory` 和页面管理共用同一个 Store/Recall 能力。前四项修改/读取最新 Store，任何
  修改都只影响未来新建 Session。
- `demo_records` 是默认本地 RecallSource，每次调用重新读取样例 JSON；`failing_demo` 只用于
  验证结构化部分失败。结果必须保留 source/reference/attributes，不能自动写入 Pinned Store。
- 页面只展示冻结 Prompt 的 part 数、Pinned revision/数量和 Source ID，以及最新 Store 状态；
  不把完整 System Prompt 放进普通 snapshot、日志或浏览器持久存储。

M4–M6 使用过的确定性模型现只保留为自动测试私有构造入口；普通 `serve` 从 M7 起固定装配真实
Provider，不提供并行的离线 CLI 模式。

## M7 Context 与真实 Provider

- 普通 `serve` 使用 `OpenAiCompatibleService` 和 DeepSeek Profile；credential 只从进程环境或
  仓库 `.env` 读取，不进入页面配置、Journal、普通日志或测试 fixture。自动测试改用私有离线
  构造入口，不读取 credential，也不访问网络。
- 每个 Session Agent 冻结同一份 DeepSeek thinking、`reasoning_effort=high`、ToolChoice 和
  Provider Options；工具续轮与上下文 continuation 都复用该配置。
- `CompactionCoordinator` 只协调完整 Journal checkpoint、`agent-context` 候选 replacement、
  原子提交与同 Agent 续跑。默认摘要预算为 1024 tokens，至少保留最近 1 个 User Turn；
  `--max-compaction-handoffs` 默认 2，达到上限后 Run 明确失败。
- `--retry-transient` 是显式开关。开启后只对 connection、timeout、rate-limit 和 unavailable 的
  建流失败追加最多 2 次尝试，间隔 250ms/1s，`Retry-After` 上限 5s；已建立流不透明重试。
- 页面只展示 Provider、模型名、上下文窗口、连接状态和调用/尝试/重试计数，不展示请求正文或
  credential。刷新仍从服务端 snapshot 校准。
- gate/barrier 自动测试证明不同 Session 可共同进入共享 ModelService；取消、失败、慢 SSE 和
  compaction replacement 均保持 Session 隔离。同一 Session 仍由活动 Run 闸串行化。

真实启动：

```bash
cargo run -p core-demo -- serve \
  --workdir . \
  --data-dir /tmp/ez-assistant-core-demo \
  --port 0 \
  --max-compaction-handoffs 2 \
  --retry-transient
```

## M8 确定性性能基线

- `examples/core_baseline.rs` 只测内存中的 ContextLayout、ToolSet 冻结、AgentEvent 消费/断开、
  双 Agent 共享 ModelService 和 SDK 相对直接 Core 的路径，不读取 `.env`、访问网络、读写用户
  数据文件或启动 Agent 工具；只调用 `rustc -vV` 记录当前工具链。
- 使用固定预热与迭代、`Instant` 和 `black_box`，逐行输出带 case、规模、迭代、总耗时、target、
  profile 与 rustc version 的 JSON。结果只用于相同环境下发现数量级回归，不定义跨硬件 SLA。
- 基线使用 `agent-testkit` dev-dependency；它不会进入 Core Demo 普通运行依赖。

```bash
cargo run -p core-demo --release --example core_baseline
```

## 验证

```bash
cargo metadata --format-version 1
cargo tree -p core-demo --depth 3
cargo check -p core-demo
cargo run -p core-demo -- --help
cargo test -p core-demo
cargo test -p agent-tools-local -- --ignored
cargo clippy -p core-demo --all-targets --all-features -- -D warnings
cargo fmt --all --check
```
