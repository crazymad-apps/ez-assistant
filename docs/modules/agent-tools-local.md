# agent-tools-local 模块约束

## 模块定位

`crates/agent-tools-local` 是真实本地文件系统与 Shell 的基础设施 Adapter。它实现
`agent-tools` 定义的能力契约，供正式 Runtime、独立开发宿主或测试在装配期选择使用。

修改前必须阅读：

- [`Rust 编程规范`](../specs/Rust编程规范.md)
- [`Agent 系统技术架构`](agent-system.md)
- [`agent-tools 模块约束`](agent-tools.md)

## 依赖边界

- 只向下依赖 `agent-tools` 和实现真实 I/O 所需的基础设施依赖。
- 不依赖 `agent-core`、`assistant-runtime`、`assistant-protocol`、Provider Adapter、
  Tauri、桌面 UI 或开发工具。
- 不定义 Tool Registry、Agent Loop、ExecutionSpec、工作模式或审批协议。
- 产品 crate 和开发宿主可以向下依赖本 crate；本 crate 不反向感知调用方。

## 职责

- 实现真实文本文件 read/list/find/search/write/edit/delete。
- 使用系统 `rg` 作为名称和内容搜索后端，不自行维护递归搜索引擎。
- 实现平台 Shell launcher、敏感环境过滤、stdout/stderr 流、输出上限、timeout、
  cancellation 和进程树清理。
- 维护单 Adapter 实例内按绝对逻辑路径协调的 mutation lock。
- 把操作系统、文件类型、编码、搜索后端和进程错误转换为 `agent-tools` 的稳定能力
  错误。
- 只实现能力 Adapter；Session path resolver、标准工具壳和 Registry 的总装由上层宿主
  负责。
- 正式产品由 `apps/runtime-host` 在顶层装配本 crate，并按单次 Run 注入已经冻结的
  `SessionPathResolver` 和 Authorizer；`assistant-runtime` 只依赖抽象端口，不直接依赖
  本地 Adapter。

## 路径与文件约束

- Adapter 接收已经 resolve 的绝对逻辑路径，不重新应用默认工作目录或授权规则。
- 权限规则匹配逻辑路径，不得把 canonical path 或 symlink 目标描述为强沙盒边界。
- read/edit/search-content 只处理 UTF-8 文本；大小、NUL、BOM 和换行语义必须显式。
- list 同时返回目标类型和独立 symlink 标记；symlink 不是与 File/Directory 平级的
  互斥目标类型。
- write/edit 可以跟随 symlink 修改目标；delete symlink 只删除链接本身。
- delete 允许普通文件、symlink 和空目录，不递归删除目录。
- write/edit/delete 在同一 Adapter 实例内按逻辑目标串行；锁不得跨无关路径或宿主
  策略判断。
- mutation 在首个副作用前检查取消；副作用开始后必须完成当前资源清理再返回。

## 搜索约束

- `rg` 通过 program + args 直接启动，不经 Shell 拼接。
- 固定使用 `--no-config`，并保持系统 `rg` 默认的 ignore、隐藏项和目录 symlink
  行为；不在 Adapter 中增加产品层搜索策略。
- query、root 和分隔符必须使用独立参数，不能把模型输入拼进命令字符串。
- exit 0/1 分别表示有结果/无结果，均为成功；exit 2、spawn 和解析失败形成稳定错误。
- 名称 NUL 记录和内容 JSON 记录必须在追加前检查单记录及累计 stdout 上限；stderr
  始终排空但只有限保留，不能先无界读取再后处理。
- 达到结果数、stdout 总量或单记录上限时返回已有部分结果和稳定截断原因；达到任一
  上限或取消时必须终止并等待 `rg` 进程树，不遗留后台进程。
- 不自动下载、安装或替换系统 `rg`。

## Shell 约束

- 模型提供完整 command；平台 launcher 只负责 program 与 fixed args，不解析、重写
  或风险评估 Shell 语法。
- POSIX 默认 `/bin/sh -c`；Windows 默认 `COMSPEC /C`，缺失时使用 `cmd.exe /C`；
  构造时允许测试或宿主覆盖。
- stdin 关闭，stdout/stderr 分离并并发排空。
- 达到合计输出上限后停止累计和推送新字节，但继续 drain，避免管道反压死锁。
- 同一绝对 deadline 和 cancellation 必须覆盖主 Shell 等待、stdout/stderr 收敛和清理；
  kill/wait 失败不能伪装成已清理。
- `managed` 在主 Shell 退出后也清理仍可管理的后代；`detached` 只有主 Shell 已退出且
  两条输出管道 EOF 后才交接，在此之前超时/取消仍清理整组。Adapter 不提供后台句柄、
  健康检查、自动停止或恢复。
- Shell 非零退出是带 exit code 的正常完成结果，不等同于 Adapter 执行失败。
- 子进程环境采用 allow/deny/override 组合并经 `env_clear` 后显式注入；默认过滤通用
  `_API_KEY`、`_TOKEN`、`_SECRET` 后缀，完整继承必须显式选择；不内置具体 Provider
  credential 名称。
- Unix 进程组无法约束主动调用 `setsid` / `setpgid` 逃离进程组的后代；这是机制限制，
  不能把本 Adapter 描述为 OS 沙箱。Windows 对应边界由 Job Object 决定。
- 不提供 stdin、TTY 或由 Session 持有的后台进程句柄。

## 不应放在本模块的内容

- Plan/Build、Ask/Auto、名单规则、审批 UI 或审计存储。
- Agent Guardrail、预算、Authorizer、Recorder 或执行终态。
- 正式 Session、Run、持久化、数据库或应用协议。
- 自动下载 `rg`、Shell AST、OS 沙盒、权限降级或容器。

## 验证

M0 骨架阶段：

```bash
cargo metadata --format-version 1
cargo tree -p agent-tools-local --depth 2
cargo check -p agent-tools-local
cargo fmt --all --check
```

实现真实能力后：

```bash
cargo test -p agent-tools-local
cargo clippy -p agent-tools-local --all-targets --all-features -- -D warnings
```

- 文件测试只使用临时目录。
- `rg` 默认测试使用脚本化假程序，真实系统 `rg` 用例标记 ignored。
- Shell 测试只运行无破坏的受控命令；等待使用有界 timeout/poll，不留下临时后台进程。
- 平台行为使用 `#[cfg(unix)]` / `#[cfg(windows)]` 分开验证。
