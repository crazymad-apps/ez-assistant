# Rust 开发规范

本规范适用于 workspace 内全部 Rust package。它约束日常编码、公共 API、异步并发、错误处理、
测试和质量门禁；crate 划分、依赖方向、状态所有权等设计问题同时遵循
[`Rust 架构设计规范`](Rust架构设计规范.md)，领域规则遵循对应的 `docs/modules/*.md`。

## 一、规范依据与用词

本规范按以下层级解释：

1. Rust 编译器、[Rust Reference](https://doc.rust-lang.org/reference/)、
   [Rust Style Guide](https://doc.rust-lang.org/stable/style-guide/)、
   [Cargo Book](https://doc.rust-lang.org/cargo/reference/) 和
   [rustdoc Book](https://doc.rust-lang.org/rustdoc/) 定义语言与工具行为；
2. 本仓库 `AGENTS.md`、架构设计规范和模块约束定义项目硬边界；
3. [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) 用于公共 API 设计；
4. rust-analyzer、Tokio、Serde、Cargo、ripgrep 等成熟项目只作为工程实践参考，
   不能覆盖前述规则。

文中的“必须/禁止”是合入要求；“应/优先”允许在有明确理由、测试和 Review 结论时例外；
“可以”表示不作统一要求。若规则冲突，先满足更高层级并在设计或代码注释中记录原因。

## 二、基本原则

- 先搜索现有实现、调用方和依赖关系，再新增类型、trait、模块或依赖。
- 正确性和可读性优先于减少行数；显式状态转换优先于隐藏在宏、析构或隐式副作用中。
- 让非法状态尽量无法构造：优先使用 newtype、enum、非零类型和经过校验的构造函数。
- 归属优先于类别：只被一个模块使用的实现就近放置，不创建 `utils.rs`、`common.rs`、
  `manager.rs` 等无稳定领域含义的杂物箱。
- 抽象必须服务于真实调用方、替换边界或测试边界；不为假想扩展提前创建公共 trait。
- workspace 默认禁止 `unsafe`，以根 `Cargo.toml` 的 `unsafe_code = "forbid"` 为准。

## 三、Workspace、package 与依赖

- workspace 共享 edition、版本、许可证、依赖版本和 lint；成员通过 `workspace = true` 继承。
- 多个 package 使用的第三方依赖版本放在根 `[workspace.dependencies]`；成员只启用自己需要的
  feature。Cargo feature 会在依赖图中合并，因此 feature 必须尽量保持可叠加，不能用 feature
  表达运行时互斥业务模式。
- 新增依赖前必须核对：标准库或既有依赖能否满足、许可证、维护状态、MSRV、原生构建成本、
  默认 feature、二进制体积和目标平台影响。
- 库 crate 不得为了方便依赖应用 crate、具体 UI、进程入口或测试宿主；dev-dependency 不能泄漏到
  正式公共 API。
- `Cargo.lock` 由 workspace 统一维护。不得手工编辑 lockfile，也不得提交 `target/` 等构建产物。
- 若项目声明 MSRV，提升 MSRV 属于兼容性变化，必须在版本设计中显式记录并验证。

参考：[Cargo Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html)、
[Cargo Features](https://doc.rust-lang.org/cargo/reference/features.html) 和
[Cargo SemVer Compatibility](https://doc.rust-lang.org/cargo/reference/semver.html)。

## 四、模块、文件与可见性

### 4.1 模块组织

- `lib.rs`、`main.rs` 和领域根模块应是代码地图：声明模块、公开少量入口并展示阅读顺序，
  不长期堆积所有实现。
- 文件按“同一职责、共同变化、共享不变量”聚合。一次需求若会迫使多个无关区域同时修改，
  说明模块边界可能错误。
- 不设机械行数上限。出现以下任一情况时必须评估拆分：
  - 文件同时承担入口编排、协议转换、I/O、状态机等多个独立职责；
  - 阅读主流程必须跨过大量边缘 helper、平台分支或测试夹具；
  - 测试代码显著淹没生产实现；
  - 私有函数形成清晰、可命名且有独立不变量的一组；
  - 文件的不同部分由不同原因、不同版本频繁修改。
- 拆分以职责命名，例如 `connection.rs`、`dispatch.rs`、`validation.rs`；不得仅按
  `part1.rs`、`helpers.rs` 或行数切片。
- 模块测试较多时使用同级 `tests.rs` 或 `module/tests.rs`；跨 crate 行为使用 `tests/` 集成测试。
  测试移动不得迫使生产项扩大为 `pub`。

### 4.2 阅读顺序

- 模块声明在前，其后依次为标准库、外部 crate、当前 crate import；由 `rustfmt` 统一格式。
- 重要公共入口和主类型放在辅助实现之前；调用者应先看到“能做什么”，再看到“如何完成”。
- 关联行为优先放在类型的 `impl` 中；只服务某一领域类型的构造、校验和状态转换应靠近该类型，
  避免散落在远端的自由函数。

### 4.3 可见性

- 使用满足调用方所需的最小可见性：私有优先，其次 `pub(super)` / `pub(crate)`，最后才是 `pub`。
- `pub` 表示稳定边界承诺，不是解决模块访问报错的快捷方式。新增公共项必须检查文档、兼容性和
  下游调用方。
- 谨慎 re-export；同一项不应存在多个惯用导入路径。Facade crate 的 re-export 必须是明确设计。

上述阅读顺序参考
[rust-analyzer Style Guide](https://rust-analyzer.github.io/book/contributing/style.html)，属于本项目采用的
工程约定，而非 Rust 语法要求。

## 五、命名与惯用 API

- 模块、文件、函数和变量使用 `snake_case`；类型和 trait 使用 `UpperCamelCase`；常量使用
  `SCREAMING_SNAKE_CASE`。
- 名称表达领域含义，避免 `Manager2`、`Data`、`Info`、`Util` 等无法说明职责的泛化名称。
- Command 表达意图，Event 表达已发生事实；布尔值使用 `is_`、`has_`、`can_`、`should_`。
- Getter 通常直接使用字段语义名，不机械添加 `get_`；转换遵循 `as_`（借用）、`to_`（有成本转换）
  和 `into_`（消费所有权）的 Rust 约定。
- 有自然 receiver 的操作使用方法；复杂构造优先 builder 或带语义的构造参数对象，避免多个相邻
  `bool`、裸字符串和同类型数字参数。
- 公共类型应实现语义成立的常见 trait，例如 `Debug`、`Clone`、`Eq`、`Hash`、`Default`；
  不能只为满足调用方便而派生错误语义。
- 智能指针只表达所有权，不通过 `Deref` 把普通领域包装伪装成另一种类型。

公共 API Review 以
[Rust API Guidelines Checklist](https://rust-lang.github.io/api-guidelines/checklist.html) 为基准。

## 六、类型、所有权与数据边界

- 应用 ID、已校验 key、token 数量、超时和状态使用有语义的类型，不在业务层传播裸 `String`、
  `usize` 或魔法数字。
- 用 enum 表达互斥状态及失败原因；不用 `bool` 同时承载结果和原因，不用 `Option<bool>` 模拟状态机。
- 构造函数必须建立类型不变量；从外部输入反序列化得到的值仍需经过语义校验，不能把
  `Deserialize` 等同于“合法”。
- 借用用于临时观察，拥有值用于跨任务、跨线程或跨生命周期保存。不要为绕过借用检查器盲目
  `clone()`；每个重要 clone 应能说明复制的是值、引用计数句柄还是不可变快照。
- `Arc` 表示共享所有权，不自动表示线程安全或状态一致；其内部可变性、锁粒度和权威写入者必须明确。
- 公共函数避免暴露内部集合的可变引用；返回只读引用、迭代器、拥有快照或受控命令。
- 泛型和 trait object 按边界选择：热点、小型且需要静态分派时可用泛型；稳定替换边界、异构集合或
  控制编译膨胀时可用 trait object。不要为每个结构体创建同名 trait。

## 七、错误、panic 与清理

- 可预期的运行时失败使用 `Result` 和具体错误类型；库层保留错误 source，应用/协议边界再转换为
  稳定、安全、可展示的信息。
- 错误 variant 表达调用方能采取的不同行动；不能把所有失败压成字符串，也不能泄漏 credential、
  prompt、路径隐私或 Provider 原始响应。
- `panic!`、`unwrap()`、`expect()` 只用于测试，或由类型/编译期保证且无法合理恢复的不变量；
  对配置、网络、文件、用户输入、模型和数据库不得使用它们代替错误处理。
- 若必须断言内部不变量，错误信息应说明被破坏的不变量，而不是只写“should work”。
- `Drop` 不执行可能失败的业务提交和长时间阻塞操作；需要确认完成的清理提供显式 `close`、
  `shutdown` 或 `flush`。
- 禁止只记录错误后返回成功；降级行为必须是契约的一部分并可被调用方观察。

## 八、异步、并发与取消

### 8.1 任务与阻塞

- 网络、模型流、异步文件 I/O 和调度使用 Tokio；CPU 密集或阻塞调用使用 `spawn_blocking`、
  专用线程或专用执行器，并设置并发上限。
- 每个 `tokio::spawn` 都必须能回答：谁拥有任务、如何取消、谁等待结束、panic 如何观察。
  不允许无所有者的永久后台任务。
- `JoinHandle` 不能随意丢弃；有意 detach 时必须在代码或架构文档中说明生命周期和失败观察方式。

### 8.2 锁与状态

- 不持有同步锁跨越 `.await`。同步锁只保护短且低竞争的纯内存临界区；需要异步独占 I/O 时优先
  使用单所有者任务加消息传递，而不是用大范围异步 Mutex 包住整个服务。
- 持锁期间不调用未知回调、Provider、工具、文件、Shell、数据库或事件发送。
- 多字段一致性必须由同一锁、不可变快照或单写入者保证，不能依赖“调用通常不会并发”。

### 8.3 通道、背压和并发上限

- 默认使用有界通道和 `Semaphore`。容量必须有来源，并明确队列满时是等待、拒绝、合并还是丢弃。
- 只有可通过权威快照恢复的观察事件可以丢弃；命令、业务事实、审批和持久化结果不得静默丢失。
- broadcast lag、receiver 关闭、oneshot sender 丢失都必须有明确语义。

### 8.4 取消与关闭

- 在 `tokio::select!` 中使用 future 前必须判断 cancellation safety；若 future 被丢弃会丢进度，
  应移出循环、持久化 future、改用安全原语或显式接受并记录损失。
- 取消是状态转换，不等于立即 `abort`。任务应在可控边界观察 token，停止产生新副作用，再完成必要清理。
- 受控关闭按三步设计：停止接收新工作、广播取消、等待已拥有任务完成或超时；关闭顺序必须测试。
- 超时应包围真正需要限制的范围，并区分排队超时、建连超时、请求超时和关闭超时。

异步规则参考 Tokio 的
[Shared State](https://tokio.rs/tokio/tutorial/shared-state)、
[`select!` Cancellation Safety](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety) 和
[Graceful Shutdown](https://tokio.rs/tokio/topics/shutdown)。

## 九、序列化、协议与兼容性

- 只在真实 wire、配置或持久化边界定义可序列化 DTO；内部领域类型不能因为方便而直接派生
  `Serialize` / `Deserialize`。
- DTO 与内部类型显式转换，借此隔离兼容、默认值、校验和 secret 脱敏。
- enum 的序列化 tag、字段名、默认值和 `Option` 语义属于兼容契约；新增 variant 前检查所有
  exhaustive 消费者。需要开放演进的公共 enum 应在设计阶段明确策略。
- 不直接序列化锁、句柄、trait object、数据库实体或 Provider 原生对象。
- 公共 Rust API、应用协议、配置 schema 和持久化格式是四种不同兼容边界，必须分别评估；
  不能用 crate 仍处于 `0.x` 作为静默破坏数据格式的理由。
- Cargo feature 应保持 additive；运行时可选行为优先使用配置或策略对象。

## 十、注释与文档

- 项目内解释性注释和 rustdoc 默认使用中文；标识符、协议字段、标准术语、外部 API 名称和必须原样
  保留的错误文本可以使用英文。不能因为代码作者能够理解实现，就省略后续维护者需要的上下文。
- crate 根使用 `//!` 说明职责、边界和主要入口；公共 API 使用 `///` 说明契约，而不是复述签名。
- 注释重点解释“为什么”、不变量、所有权、背压、取消安全和兼容取舍；代码已经清楚表达的“做什么”
  不需要逐行翻译。
- 下列位置必须有必要的中文注释或模块文档：跨 crate 能力边界、权威状态与所有权、异步任务生命周期、
  取消与背压、安全与权限、序列化兼容、迁移与恢复、非显然的执行顺序或失败语义，以及平台兼容绕行。
- 复杂模块应有导读；主流程和危险边界前应有高价值注释。注释过时视为缺陷。
- 公共 fallible API 的 rustdoc 应包含 `# Errors`；可能 panic 的公共 API 包含 `# Panics`；
  若未来允许 unsafe，则必须包含 `# Safety` 并写清调用者责任。
- 示例应能作为 doctest 编译；示例优先使用 `?`，不鼓励复制后会掩盖错误的 `unwrap()`。

参考 [rustdoc: What to include](https://doc.rust-lang.org/stable/rustdoc/write-documentation/what-to-include.html)
和 [Rust API Guidelines: Documentation](https://rust-lang.github.io/api-guidelines/documentation.html)。

## 十一、测试

- 测试放在最能观察不变量的边界：纯函数和状态转换用单元测试，crate 公共行为用集成测试，
  wire/配置格式用 round-trip 或快照测试，跨进程只保留少量关键端到端测试。
- 优先测试可观察行为、状态转换和失败恢复，不绑定私有函数调用顺序；重构不应迫使大量无关测试改写。
- 异步测试使用 channel、barrier、`Notify`、可控时钟或 fake，不用不稳定 sleep 猜测顺序。
- 文件测试使用临时目录；网络优先 loopback/fake；默认测试不得依赖真实 credential、外部服务、
  用户目录或不可控时钟。
- 修复缺陷时先补能稳定复现的测试；并发、取消、背压、shutdown 和 secret 泄漏必须有回归覆盖。
- 测试 helper 属于测试基础设施；只有两个以上 crate 稳定复用时才提升到 `agent-testkit` 等共享 crate。

## 十二、质量门禁与 Review 清单

最小验证按改动风险选择，并在交付时说明实际执行和未执行项：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

涉及 feature、平台或编译边界时补充：

```bash
cargo test --workspace --all-features
cargo test --workspace --no-default-features
cargo tree -e features
cargo doc --workspace --no-deps
```

Review 至少核对：

- 新代码属于正确 crate 和模块，依赖方向没有反转；
- 公共可见性、trait、泛型、clone 和依赖都有真实理由；
- 错误、取消、超时、背压和 shutdown 路径可观察且经过测试；
- 序列化和日志不泄漏 secret，兼容边界已识别；
- 主流程可从上到下阅读，边缘 helper 和测试没有淹没核心编排；
- 必要的中文注释解释了不变量和取舍，且没有逐行翻译代码、过时说明或中英文重复噪声；
- 验证范围与风险相称。

## 十三、参考项目的采用边界

- [rust-analyzer Architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)：
  采用其“明确 API Boundary、记录 Architecture Invariant、隔离序列化”的方法；不照搬其编译器领域结构。
- [Tokio](https://github.com/tokio-rs/tokio)：采用其任务取消、背压和受控关闭指导；不把所有共享状态
  机械改为 actor 或异步 Mutex。
- [Serde](https://github.com/serde-rs/serde)：参考其核心契约、派生实现和测试套件分离；本项目不以发布
  crates.io 库为目标时，不机械复制其 package 数量。
- [ripgrep](https://github.com/BurntSushi/ripgrep)：参考其库能力与 CLI 装配分离、跨平台测试和稳定工具链
  取舍；不复制其搜索领域优化结构。

这些项目用于校准工程判断，不构成本仓库的隐藏依赖。具体规则仍以本文件、架构设计规范和模块约束为准。
