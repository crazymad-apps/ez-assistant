# ez-assistant

本地优先的桌面 AI 助手。

## 仓库结构

```text
apps/
  desktop/              Tauri 2 + Vanilla TypeScript 桌面应用
crates/
  agent-core/           Agent 推理循环、模型与工具抽象
  assistant-protocol/   UI、Runtime 与 Agent 之间的共享数据类型
  assistant-runtime/    多会话、Run 调度、定时任务与配置加载
```

当前采用单进程模块化架构：以上 Rust crate 最终共同运行在 Tauri 进程中，不引入 sidecar 或多进程通信。

## 开发约束

开发前从 [`AGENTS.md`](AGENTS.md) 开始阅读，并按改动范围继续阅读 `docs/specs/` 和 `docs/modules/` 下的对应规范。长期架构决定记录在 [`docs/重要决策与变更记录.md`](docs/重要决策与变更记录.md)。

## 启动桌面应用

```bash
cd apps/desktop
npm install
npm run tauri dev
```

## 检查

```bash
cargo check --workspace
```
