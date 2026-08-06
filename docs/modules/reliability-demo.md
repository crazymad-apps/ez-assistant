# reliability-demo 模块约束

## 模块定位

`tools/reliability-demo` 是开发者显式启动的 v0.6.0 可靠性验证宿主，用于装配真实或脚本模型、
Provider wire 观察、模型建立重试、完整 Trace Collector 和离线分层回放。

它不属于产品进程、正式 `assistant-runtime`、sidecar、daemon 或本地服务。修改前必须阅读：

- [`Rust 编程规范`](../specs/Rust编程规范.md)
- [`Agent 系统技术架构`](agent-system.md)
- [`agent-model 模块约束`](agent-model.md)
- [`OpenAI-compatible Provider 模块约束`](agent-provider-openai-compatible.md)
- 当前版本功能设计、技术方案和开发计划

## 职责

- 用真实 OpenAI-compatible Provider 完成一次 AgentExecution，并收集 Provider、Model、Agent
  和宿主的原生事实。
- 在用户显式指定的数据目录中私有实现版本化 JSONL、Complete/Incomplete 和严格 Loader。
- 提供 Wire Replay、Model Replay 和只读 Timeline，验证断网条件下的确定性复现。
- 使用确定性 fixture 验证建立失败、有限重试、Retry-After、取消、流中断、记录失败和并发隔离。

## 核心约束

- 产品 crate 不得依赖本工具；本工具只向下依赖 Agent crate 和 Provider Adapter。
- `agent-testkit` 只能作为 dev-dependency，不能进入 binary 的普通运行依赖。
- Trace 文件、Provider metadata、宿主事件、CLI 参数和 Replay 实现全部保持 binary 私有，不进入
  `assistant-protocol` 或正式 Runtime 契约。
- 只有 `record` 命令可以加载 `.env`、credential 和真实网络；Replay、Timeline、help 与自动测试
  不得读取 credential 或访问网络。
- credential、Authorization、Cookie、代理认证和 URL 认证数据不得进入 Trace、错误、控制台或
  fixture。完整 prompt、reasoning、工具参数和响应正文只在显式 Full Trace 中保存，按高敏数据
  处理。
- Collector 有界且不阻塞模型流。队列、序列化、文件或容量失败只把 Trace 标为 Incomplete，
  不改变模型或 Agent 结果。
- Wire/Model Replay 与 Timeline 不连接真实工具、Authorizer、Recorder 或 Journal，不执行历史
  文件、Shell、Memory 或其他副作用。
- 建立重试只包装 `ModelService::stream()` 的直接错误；事件流建立后不透明重试。
- 真实 DeepSeek record 的 `ExecutionSpec` 显式冻结 reasoning 和
  `thinking: { type: enabled }`；Observed/Retrying ModelService 不改写请求配置。
- 自动测试使用 tempfile、scripted model/transport、gate 和暂停时间，不访问用户目录、应用数据
  或真实 Provider。
- 不新增 HTTP server、前端页面、数据库、正式 Session/Run 类型或后台常驻进程。

## 私有文件与命令边界

- 数据目录必须由调用方显式提供，不读取或写入桌面应用数据目录。
- Trace 先写 `<trace-id>.incomplete.jsonl`，只有完成尾和 flush 成功后才改名为
  `<trace-id>.jsonl`；Incomplete 数据只允许 Timeline 查看，不能精确回放。
- CLI 提供 `record`、`replay-wire`、`replay-model` 和 `timeline`。只有 `record` 构造真实
  Provider、Core、固定无副作用工具和私有内存 Journal；三个读取命令均不装配这些执行入口。
- 普通控制台默认只显示类型、计数、关联和完整性，不打印 Full Trace 正文。

## 启动与人工闭环

`record` 支持以下环境变量：

- `DEEPSEEK_API_KEY`：必需；只进入 Bearer credential，不进入 Trace 或控制台；
- `DEEPSEEK_BASE_URL`：可选，默认 `https://api.deepseek.com`；含 userinfo、query 或 fragment
  的值会在创建 Trace 前被拒绝；
- `DEEPSEEK_MODEL`：可选，默认 `deepseek-v4-flash`；
- `DEEPSEEK_CONTEXT_WINDOW_TOKENS`：可选，默认 `128000`，必须为正整数。

先用专用临时目录完成一次真实调用。命令会产生实际模型费用，并且 Trace 包含完整 prompt、
reasoning、工具参数与响应正文：

```bash
demo_trace_dir="$(mktemp -d)"
printf '%s\n' '请用一句话说明可靠回放的意义。' | \
  cargo run -p reliability-demo -- record --data-dir "$demo_trace_dir"
```

`record` 固定要求模型调用一次 `reliability_probe`。该工具只返回确定性 JSON，不访问文件、Shell、
网络、记忆或产品数据；Demo 的显式预算为最多 4 个模型 Step 和 1 次工具执行。按 Ctrl-C 会请求
取消当前 AgentExecution，并把取消事实写入仍可用的 Trace。

命令结束会打印 Trace 的绝对路径。对同一个 Complete 文件执行：

```bash
cargo run -p reliability-demo -- timeline --trace <COMPLETE_TRACE>
env -u DEEPSEEK_API_KEY cargo run -p reliability-demo -- \
  replay-wire --trace <COMPLETE_TRACE>
env -u DEEPSEEK_API_KEY cargo run -p reliability-demo -- \
  replay-model --trace <COMPLETE_TRACE>
```

显式有限重试示例：

```bash
printf '%s\n' '执行一次可靠性验证。' | cargo run -p reliability-demo -- record \
  --data-dir "$demo_trace_dir" \
  --retry-on connection --retry-on timeout --retry-on rate-limited \
  --retry-delay-ms 250 --retry-delay-ms 1000 --max-retry-after-ms 5000
```

未同时提供 retry reason、至少一个 delay 和最大 `Retry-After` 时，CLI 在发起执行前拒绝配置。
不提供任何 retry 参数时不会装配 `RetryingModelService`。

## 验证

基础与闭环验证：

```bash
cargo metadata --format-version 1
cargo tree -p reliability-demo --depth 2
cargo check -p reliability-demo
cargo run -p reliability-demo -- --help
cargo fmt --all --check
cargo test -p reliability-demo
cargo clippy -p reliability-demo --all-targets --all-features -- -D warnings
```
