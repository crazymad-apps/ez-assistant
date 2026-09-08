# ez-assistant

本地优先的 AI 助手，支持桌面与 Web 客户端。你可以在本机工作，也可以连接另一台电脑上的
Runtime，使用同一套会话、工作空间、文件与终端能力。

**当前版本：v0.25.0** · [下载正式版](https://github.com/crazymad-apps/ez-assistant/releases/latest) ·
[发布说明](docs/versions/v0.25.0/发布说明.md) · [版本路线](docs/版本管理.md)

## 主要能力

- **会话与任务**：多会话、工作空间、主控会话与子 Agent，支持计划／构建模式、工具审批、Todo 和 Goal。
- **模型与上下文**：OpenAI-compatible 模型接入、上下文压缩、固定记忆与历史召回。
- **Skills 与 MCP**：查看当前技能和工具服务，支持 `/skill refresh`、`/mcp refresh`。
  刷新会插入一条 user role 消息，不触发新的 Run，也不改写已经冻结的系统提示词。
- **资源工作区**：浏览 Host 目录，查看代码、Markdown、图片与 PDF，上传和下载文件。
  Desktop 提供内嵌浏览器；Desktop 和 Web 均可使用 Host 上的终端。
- **本机与远端连接**：Desktop 入口页选择本机或其他 Runtime，设置中可切换目标；
  Web 可直接访问 Host，桌面标题栏也提供打开 Web 的快捷入口。
- **按需启动**：历史会话按需加载，MCP 在后台初始化。重启后不自动续跑历史任务，
  待审批状态不恢复，由用户手动继续工作。

## 下载与开始使用

当前提供 **macOS Apple Silicon（arm64）** 安装包，App、随包 Runtime 和 DMG 均已完成
Developer ID 签名；App 与 DMG 已通过 Apple 公证并写入票据。

1. 从 [GitHub Releases](https://github.com/crazymad-apps/ez-assistant/releases/latest) 下载 DMG，
   打开后将 **EZ Assistant** 拖入 **Applications**。
2. 启动应用，在入口页选择 **本机 Runtime**，等待就绪后进入工作空间。
3. 在设置中配置模型与凭据，选择工作空间并创建会话。

正式应用默认使用 `~/.ez-assistant` 保存 Runtime 数据。桌面应用随包提供 Host 和 Web 页面，
使用安装包不需要另起前端开发服务。

### 通过 Web 或另一台 Desktop 连接

在目标 Host 的 **设置 → Runtime → 访问设置** 中设置密码，并开启“允许其他设备连接”。
非本地访问默认关闭，空密码不能开启。

本机与其他设备共用一个监听，默认端口为 **7240**。使用默认 HTTP 配置时：

- 本机 Web：`http://127.0.0.1:7240`，也可点击 Desktop 标题栏的打开 Web 图标。
- 其他设备 Web：`http://<Host 的局域网 IP>:7240`，输入密码登录。
- 其他 Desktop：入口页选择 **其他 Runtime**，填写同一地址和密码。

仅通过 IP 访问时，“允许的域名”可以留空；使用域名时填写域名，不带协议和端口。
HTTP 和 HTTPS 均可配置；HTTPS 需要有效且受客户端信任的证书。
Desktop 一次连接一个 Host，进入后通过 Runtime 设置切换，不自动连接上次目标。

文件目录和终端命令都属于**当前连接的 Host**，可能与浏览器或 Desktop 所在电脑不同。
Web 上传选择客户端文件，工作空间目录则从 Host 选择。Shell 以 Host 当前用户权限运行，
工作目录不构成操作系统沙盒。

刷新 Web 页面或连接断开会回收其所属终端，不恢复旧 Shell 或重放命令。
关闭窗口、退出客户端与停止 Runtime 是不同动作；停止 Runtime 使用应用中的明确操作。

## 从源码开发

当前桌面开发与发布验证环境为 macOS arm64。准备 Rust／Cargo、Node.js／npm，以及可用的
Xcode 或 Command Line Tools，然后在仓库中执行：

```bash
cd apps/desktop
npm ci
npm run tauri -- dev
```

脚本会先构建前端和 Host，再启动 Vite 与 Tauri。开发前端使用 `1420` 端口，重复启动前先退出
原开发进程。Web 页面内嵌在 Host 中，前端改动后需重新构建并重启对应 Host 才会更新其 Web 页面。

开发脚本默认也使用 `~/.ez-assistant`，会访问该目录中的现有数据；Dev 应用标识仅隔离桌面偏好
与 WebView 存储。需要独立数据时，可显式指定 Runtime Home：

```bash
EZ_ASSISTANT_RUNTIME_HOME="$HOME/.ez-assistant-playground" npm run tauri -- dev
```

多 Host 同时运行还需配置不同的监听端口。自动化测试使用临时 Runtime Home，不向用户目录写入
测试数据。开发页面与 Host 应来自匹配的开发构建；不要直接复用安装版 Host 作为 Vite 的后端。

### 构建与验证

以下命令从仓库根执行：

```bash
# Rust 格式、静态检查与测试
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

# 前端检查与测试
npm --prefix apps/desktop run lint
npm --prefix apps/desktop test
npm --prefix apps/desktop run build

# Desktop 壳与内嵌 Web 集成测试
npm --prefix apps/desktop run test:e2e
npm --prefix apps/desktop run test:e2e:web

# 本地 App / DMG 构建
npm --prefix apps/desktop run tauri -- build --bundles app,dmg
```

本地打包不等于正式签名、公证发布。正式 macOS 发布使用 `npm run release:macos`，
凭据在仓库外配置，步骤见 [macOS 发布流程](docs/release/macOS发布流程.md)。

开发前请阅读 [AGENTS.md](AGENTS.md)，再按改动范围阅读 `docs/specs/` 和 `docs/modules/`
中的约束。实际测试覆盖、平台边界及未执行项目见 [v0.25.0 验收记录](docs/versions/v0.25.0/验收记录.md)。

## 架构与仓库结构

正式产品采用 **Desktop 与 Runtime 双进程、Runtime 内部模块化** 的架构：

```text
Desktop / Web
    │ HTTP Command · SSE · Streaming Upload · WS/WSS 终端
    ▼
Runtime Host
    └─ assistant-runtime          会话、Run、配置、调度与持久化
        └─ Agent 能力与 Adapter   推理、上下文、记忆、模型与工具
```

Tauri 负责 WebView、桌面平台适配和 Runtime Client；业务权威状态由 Runtime 持有。
Session 在同一 Host 内通过异步任务并发，不为每个会话创建操作系统进程。
Host 随应用交付，独立进程不等于独立分发产品或系统 daemon。

```text
apps/
  desktop/                  Tauri 2 + React / MobX / Sass，桌面与 Web 共用前端
  runtime-host/             正式 Runtime 进程、HTTP 接入、认证、文件与 PTY
crates/
  assistant-protocol/       跨层 DTO、命令、事件与标识
  assistant-runtime/        产品业务与持久化编排
  agent-sdk/                Agent 能力装配的薄 Facade
  agent-core/               单次 Agent 执行与工具循环
  agent-context/            上下文窗口、历史布局与压缩策略
  agent-memory/             固定记忆与 RecallSource 契约
  agent-model/              Provider-neutral 模型调用契约
  agent-openai-compatible/  OpenAI-compatible Adapter
  agent-tools/              工具 SPI、Registry 与能力契约
  agent-tools-local/        本地文件与 Shell Adapter
  agent-types/              规范消息与工具值类型
  agent-testkit/            确定性测试支持
tools/
  core-demo/                Agent 综合能力验证
  runtime-harness/          Runtime／Core 版本验证宿主
  safety-demo/              安全策略与本地工具验证
  memory-demo/              记忆能力验证
  reliability-demo/         Trace、重试与回放验证
  debug-viewer/             独立调试事件查看器
  device-simulator/         智能终端协议模拟器
docs/                      规范、模块约束、版本路线与归档
```

具体模型、本地文件／Shell 和持久化由上层装配，`agent-core` 不依赖 UI 或应用协议。
详细边界见 [Agent 系统技术架构](docs/modules/agent-system.md)。

## Agent SDK 与验证工具

`agent-sdk` 提供可嵌入其他宿主的 Agent 装配入口。模型、System Prompt、Context Window、
ToolSet 和执行配置在 Agent 内冻结；动态 Conversation、Run 和 Journal 由宿主持有。

可以直接运行两个离线例子：

```bash
cargo run -p agent-sdk --example minimal
cargo run -p agent-sdk --example custom_controls
```

使用方式与扩展边界见 [Agent SDK 导读](docs/Agent-SDK导读.md)。真实模型与工具综合验证见
[Core Demo](docs/modules/core-demo.md)，设备联调见 [智能终端模拟器](tools/device-simulator/README.md)。
`tools/` 下的验证宿主各自按需启动，不属于正式产品进程，不用于替代产品验收。

## 后续方向

v0.25.1 计划推进无图形 CLI 客户端与配套部署管理；后续分别讨论 Runtime 组网、中继与 P2P，
以及原生手机 App。它们尚未包含在 v0.25.0 中，具体状态以 [版本管理](docs/版本管理.md)为准。
