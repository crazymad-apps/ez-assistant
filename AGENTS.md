# ez-assistant

本地优先的桌面 AI 助手 Monorepo。桌面端基于 Tauri 2，Agent Core 与应用 Runtime 使用 Rust 重写并运行在同一个 Tauri 进程内。

## 一、项目结构

```text
.
├── apps/
│   └── desktop/                 # Tauri 桌面应用（Vanilla TypeScript + Vite + Rust）
├── crates/
│   ├── agent-core/              # Agent 推理循环、模型与工具抽象
│   ├── agent-context/           # 共享上下文窗口判断、布局、校验与压缩策略
│   ├── agent-model/             # Provider-neutral 单次模型调用契约
│   ├── agent-memory/            # Pinned Memory、RecallSource 与协调契约
│   ├── agent-provider-openai-compatible/ # OpenAI-compatible Adapter
│   ├── agent-testkit/           # Agent 确定性测试支持
│   ├── agent-tools/             # Agent 工具 SPI、Registry/Dispatcher、能力契约与标准工具壳
│   ├── agent-tools-local/       # 真实本地文件与 Shell 基础设施 Adapter
│   ├── agent-types/             # Provider-neutral 规范类型
│   ├── assistant-protocol/      # 跨层共享 DTO、事件与标识类型
│   └── assistant-runtime/       # 会话、Run、调度、配置与持久化编排
├── tools/
│   ├── debug-viewer/            # 调试查看器（独立开发工具：POST 接收 + SSE 广播 + 静态页）
│   ├── memory-demo/             # 记忆能力与私有本地实现的独立验证宿主
│   ├── reliability-demo/        # 完整录制、有限重试与分层回放的独立验证宿主
│   ├── runtime-harness/         # 版本验证宿主（独立按需运行的临时 Runtime）
│   └── safety-demo/             # 安全策略与真实工具执行的独立验证宿主
├── docs/
│   ├── specs/                   # 开发流程、Rust/前端规范、进度模板
│   ├── modules/                 # 各应用/crate 专属约束
│   ├── inputs/                  # 临时想法与需求记录池，不作为流程主线
│   ├── design/                  # 当前版本的在途设计与开发计划
│   ├── versions/                # 已完成/暂停版本的只读归档
│   ├── resources/               # 外部资料与样例
│   ├── 版本管理.md
│   └── 重要决策与变更记录.md
└── Cargo.toml                   # Rust workspace
```

## 二、动手前必读

任何代码改动前都必须读取：

1. [`docs/specs/开发流程规范.md`](docs/specs/开发流程规范.md)。
2. 对应语言规范。
3. 对应模块约束。

涉及 Agent 执行、模型、工具、记忆、安全、上下文、Run 或会话持久化时，还必须读取 [`Agent 系统技术架构`](docs/modules/agent-system.md)。

| 改动范围 | 语言规范 | 模块约束 |
| --- | --- | --- |
| `apps/desktop/src/**` | [`前端编程规范.md`](docs/specs/前端编程规范.md) | [`desktop.md`](docs/modules/desktop.md) |
| `apps/desktop/src-tauri/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`desktop.md`](docs/modules/desktop.md) |
| `crates/agent-core/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-core.md`](docs/modules/agent-core.md) |
| `crates/agent-context/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-context.md`](docs/modules/agent-context.md) |
| `crates/agent-memory/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-memory.md`](docs/modules/agent-memory.md) |
| `crates/agent-types/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-types.md`](docs/modules/agent-types.md) |
| `crates/agent-model/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-model.md`](docs/modules/agent-model.md) |
| `crates/agent-provider-openai-compatible/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-provider-openai-compatible.md`](docs/modules/agent-provider-openai-compatible.md) |
| `crates/agent-testkit/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-testkit.md`](docs/modules/agent-testkit.md) |
| `crates/agent-tools/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-tools.md`](docs/modules/agent-tools.md) |
| `crates/agent-tools-local/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`agent-tools-local.md`](docs/modules/agent-tools-local.md) |
| `crates/assistant-runtime/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`assistant-runtime.md`](docs/modules/assistant-runtime.md) |
| `crates/assistant-protocol/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`assistant-protocol.md`](docs/modules/assistant-protocol.md) |
| `tools/debug-viewer/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`debug-viewer.md`](docs/modules/debug-viewer.md) |
| `tools/memory-demo/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`memory-demo.md`](docs/modules/memory-demo.md) |
| `tools/reliability-demo/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`reliability-demo.md`](docs/modules/reliability-demo.md) |
| `tools/runtime-harness/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`runtime-harness.md`](docs/modules/runtime-harness.md) |
| `tools/safety-demo/**` | [`Rust编程规范.md`](docs/specs/Rust编程规范.md) | [`safety-demo.md`](docs/modules/safety-demo.md) |

根 `AGENTS.md`、`docs/specs/` 与 `docs/modules/` 是本仓库开发约束的唯一来源。不要在源码目录重复创建相互冲突的约束文件。

## 三、架构硬约束

- 当前采用**单进程模块化架构**。Tauri、Assistant Runtime 和 Agent Core 共同运行于 Tauri Rust 进程；未经明确架构决策，不引入 sidecar、daemon、本地 HTTP 服务或常驻 Worker 进程。
- `tools/debug-viewer` 是独立开发工具，不属于产品进程；产品进程只向它建立出向连接（POST 推送调试事件），自身不监听调试端口（决策见 `docs/重要决策与变更记录.md`）。
- `tools/runtime-harness` 是开发者显式启动的版本验证宿主，不属于产品进程或正式 `assistant-runtime`，不得把其临时 Session、Run、Journal 或 CLI 语义视为产品契约。
- `tools/memory-demo` 是开发者显式启动的记忆验证宿主；其 JSON Store、RecallSource、
  Session、Journal 和 CLI 均为私有验证实现，不属于正式 Runtime 或产品协议。
- `tools/reliability-demo` 是开发者显式启动的可靠性验证宿主；其 Trace JSONL、Provider
  metadata、Replay、Timeline、宿主事件和 CLI 均为私有验证实现，不属于正式 Runtime 或
  产品协议。只有显式 record 入口可以访问真实 Provider，离线回放不得执行历史工具副作用。
- `tools/safety-demo` 是开发者显式启动的安全验证宿主；它只在 loopback 上提供无访问
  令牌的临时 HTTP 页面，并通过 Host、Origin 和 POST 来源校验降低跨站误触风险；它不
  属于产品进程、正式 `assistant-runtime`、sidecar 或 daemon，其私有 Session、Run、
  审批、审计和 HTTP DTO 不得成为产品契约。
- 关闭窗口与退出进程是两个动作；后台任务的生命周期属于 Assistant Runtime，不得绑定 WebView 页面生命周期。
- UI 只发送用户意图和展示事件，不持有会话、Run、调度或 Agent Loop 的权威业务状态。
- `assistant-runtime` 持有会话、Run、定时任务、配置、持久化和并发调度；`agent-core` 只负责单次 Agent 执行能力。
- 不同会话可以并发；同一会话中会改变上下文的 Run 默认串行。使用 Tokio 异步任务，不为历史会话绑定操作系统线程。
- `assistant-protocol` 必须保持轻量，只承载跨层契约；不得依赖 Tauri、具体模型、数据库或 Runtime 实现。
- 执行依赖方向保持为：`desktop -> assistant-runtime -> agent-core -> agent-tools -> agent-types`；
  `agent-core` 与 `assistant-runtime` 可共同向下依赖 `agent-context`，
  `agent-context` 只依赖 `agent-model` 与 `agent-types`。应用协议由 `desktop` 和
  `assistant-runtime` 依赖 `assistant-protocol`。Agent Core 内部模型、对话和执行
  类型不依赖应用协议。`agent-memory` 只承载实现无关的记忆领域与能力契约，
  `agent-tools -> agent-memory` 提供普通记忆工具壳；`agent-tools-local -> agent-tools`。
  `safety-demo`、`memory-demo` 与 `reliability-demo` 只作为顶层开发工具向下依赖 Agent crate 与所需
  Adapter；所有依赖禁止反向。

## 四、Agent 工具边界

- 文件读取、列目录、内容搜索、写入、删除和局部编辑（edit）统一归入文件能力；`grep`/`rg` 的语义由 `FileSystemTool::search` 承载，`rg` 仅可作为内部实现。
- Agent Core 不直接访问 `std::fs`、`tokio::fs` 或启动 `rg`；它只能依赖工具 trait。
  无副作用的词法路径解析属于工具 resolve；具体文件状态、搜索后端与读写机制
  属于装配的文件能力实现，本版本的本地实现位于 `agent-tools-local`。Shell
  进程与环境过滤同样由 `agent-tools-local` 实现；权限、确认和审计由 Runtime
  或其他上层宿主装配。
- Shell 是独立的一等工具，不按每条系统命令枚举专用工具。结构化文件工具与 Shell 并存，不能用 Shell 取代所有文件能力。
- Shell 子进程以当前用户权限运行，可以绕过应用层文件授权。任何 UI 和文档都不得把“工作目录”表述为强沙盒。
- Shell 必须支持权限模式、完整命令展示、流式 stdout/stderr、超时、取消、输出上限和审计；敏感环境变量默认不传递给子进程。
- 启动 Shell 命令产生子进程属于工具执行实现，不等同于引入多进程应用架构。

## 五、实现与验证原则

- 先搜索现有实现和依赖关系，再新增模块、trait 或公共类型。
- 归属优先于类别：单模块私有类型就近放置；只有两个及以上模块稳定复用时才上提。
- 不为假想需求提前抽象；公共 trait 必须有明确调用方或替换边界。
- 库代码不得依赖 UI；Tauri command 保持薄，只做参数转换、调用 Runtime 和转发事件。
- 修改协议类型时必须检查所有生产者、消费者、序列化兼容和持久化影响。
- 禁止手改 `target/`、`dist/`、生成图标和其他构建产物；源图标为 `apps/desktop/app-icon.svg`。
- 验证按最小有效范围执行，最终说明已执行、未执行及原因。

常用验证命令：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

cd apps/desktop
npm run build
npm run tauri -- build --no-bundle
```

仅文档改动不要求构建应用，但必须检查链接、路径和文档之间是否矛盾。

## 六、开发流程与 Git

- 项目以版本为开发主线，遵循“功能设计 → 技术方案与界面交互设计指导（按需）→ 开发计划 → 分里程碑实现与验证 → 版本验收 → 版本归档”主流程；单个事项先归入当前版本，不机械创建独立需求流程。具体遵循 [`开发流程规范.md`](docs/specs/开发流程规范.md)。
- 每个版本里程碑完成实现与验证后必须停止并向用户汇报；只有获得用户明确确认，才能进入下一里程碑。版本计划确认不等于后续里程碑自动获批。
- `docs/开发进度.md` 是按当前版本和里程碑组织的本地恢复文件与过程记录，已被 Git 忽略；版本归档时随版本移入归档目录一并提交；共享事实写入功能设计、开发计划、版本台账、归档或重要决策记录。
- 不主动执行 `git commit` 或 `git push`。用户明确要求提交准备时，只检查改动、暂存指定文件并草拟 commit message。
- 保留用户已有改动；不要重置、覆盖或顺手清理与当前任务无关的工作树内容。

## 七、数据库操作安全

- 任何数据库操作必须分步骤执行并反复核验，不能把目标结果授权理解为允许跳过操作前确认。
- 操作前明确核对源库与目标库的主机、端口、数据库名、表和预期影响范围，并确认应用实际读取的表。
- 删除、清空、覆盖、恢复、迁移或批量更新前必须再次取得明确确认；确认前只允许只读检查和制定方案。
- 破坏性操作前创建独立备份，并实际验证备份存在、可读及关键表精确数量有效。
- 优先使用事务；无法完整回滚时，操作前说明不可回滚步骤和恢复方案。
- 任一步骤报错、影响行数异常或验证不符时立即停止，不自行扩大操作范围或连续补救。
- 操作后逐表核对精确数量与关键字段，并区分数据库、文件、缓存和接口展示状态。
- 汇报必须说明实际主机、数据库、表、影响行数、备份位置和验证结果。
