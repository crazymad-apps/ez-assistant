# memory-demo 模块约束

## 模块定位

`tools/memory-demo` 是开发者显式启动的 v0.5.0 记忆验证宿主。它可以装配真实 Provider、
Agent Core、标准记忆工具以及私有本地 Store/RecallSource，但不属于产品进程、正式
`assistant-runtime`、sidecar 或 daemon。

修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、
[`agent-memory 模块约束`](agent-memory.md)和
[`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- 通过 CLI 创建或恢复临时 Session，并运行一次一个的 AgentExecution。
- 私有实现版本化 JSON Pinned Store、本地 RecallSource、Session 文件和 Journal。
- 装配标准 Pinned Memory/Recall 工具、显式 Authorizer 和真实 Provider。
- 验证当前会话 Prompt 冻结、新会话读取最新 Store、重启恢复原快照和召回来源追溯。
- 提供受控失败 Source，验证部分失败不破坏其他有效结果和 Agent Loop。

## 核心约束

- 所有文件格式、Session ID、Journal、CLI 命令和私有类型都只是 Demo 契约，不进入
  `assistant-protocol` 或正式 Runtime。
- 不创建可复用的 `agent-memory-local`；本地文件实现就近保持在本 binary 内。
- 使用调用方指定的数据目录，不读取或修改用户真实应用数据。
- 权威 JSON 更新采用同目录临时文件写入、flush、sync 和 persist；成功前不替换内存状态。
- 同一 Store 文件只支持单进程、单 Demo writer，不承诺跨进程锁或网络文件系统语义。
- 自动测试使用 tempfile、fake model 和受控 Source，不依赖网络或 credential。
- 只有 `chat` 子命令加载 Provider credential；help 和自动测试不得加载或输出 credential。
- 不新增 Web server、静态页面、产品 Runtime 接口或后台常驻进程。
- 不实现自动观察、提炼、整合、淘汰、Embedding 或向量搜索。

## 私有持久化边界

- `atomic_json` 只服务于本 Demo 的版本化 JSON 文件，采用目标同目录临时文件、flush、
  `sync_all` 和 persist；不是 workspace 通用存储组件。
- `DemoPinnedMemoryStore` 只承诺单进程单 writer；修改在副本上完成，原子持久化成功后才
  替换进程内状态。
- `DemoRecallSource` 每次调用重新读取文件，按 Unicode 空白 term 线性匹配；它不是生产级
  索引或语义检索实现。
- Session 创建读取最新 Store 并冻结最终 `SystemPromptSnapshot`；恢复、分支和 continuation
  只复制 Session 成品，不重新读取 Store 构造 Prompt。
- Demo Journal 的 pending exchange 单独持久化；只有完整 Tool Result 批次原子提交成功后，
  Assistant Tool Call 与 Tool Result 才一起进入 `ConversationSnapshot`。
- CLI 入口固定为 `memory-demo chat --data-dir <DIR> [--session <ID>]`；只有该入口加载
  `.env` 和 Provider credential，`--help` 与自动测试保持离线。
- 首次使用的数据目录只在 `recall-records.json` 不存在时写入可审阅样例，任何已有文件都
  不会被初始化流程覆盖。
- `demo_records` 是默认本地 Source；`failing_demo` 是显式受控失败 Source。两者用途写入
  System Prompt，不动态修改 `recall_memory` Schema。
- 五个标准记忆工具通过普通 Registry、AllowAllAuthorizer、ExecutionRecorder 和 Agent Loop
  工作；AllowAll 只是 Demo 的明确装配选择。
- `/state` 是唯一主动展示完整冻结 Pinned Part 与最新 Store 正文的观察入口；默认活动日志
  不打印工具参数、reasoning 正文或完整记忆内容。
- `/new` 只在无活动 Run 时创建读取最新 Store 的 Session；`/quit` 在活动 Run 中先取消并
  等待 Core 收敛。恢复时遗留 pending exchange 会先补齐明确的 unknown/error Tool Result。

## 启动

```bash
cargo run -p memory-demo -- chat --data-dir <TEMP_DATA_DIR>
cargo run -p memory-demo -- chat --data-dir <TEMP_DATA_DIR> --session <SESSION_ID>
```

数据目录固定包含 `pinned-memory.json`、`recall-records.json` 和
`sessions/<session-id>.json`；人工验证必须使用专门创建的临时目录。

## 验证

```bash
cargo test -p memory-demo
cargo clippy -p memory-demo --all-targets --all-features -- -D warnings
cargo fmt --all --check
```
