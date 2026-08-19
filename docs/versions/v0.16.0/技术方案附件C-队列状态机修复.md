# v0.16.0 技术方案附件 C：中断后队列暂停状态机修复

## 一、附件信息

- 状态：已确认（2026-08-18）
- 主方案：[`v0.16.0 技术方案`](技术方案.md)
- 功能基线：[`v0.16.0 功能设计`](功能设计.md)
- 问题来源：用户报告的“手动中断后新消息只入队不执行”；临时修复记录已被本文完整吸收并清理。

本文只修复 Session 输入队列门禁，不改变“不同 Session 并发、同一 Session 中改变 Conversation 的
父 Run 串行”的总模型。

## 二、根因

当前实现已经有 `queue_paused_by_user` 和 `resume_required` 两个字段，但多个操作仍成对写入：

- 用户中断把两者都设为 `true`；
- `submit_input` 不区分暂停来源；
- 行级恢复借助 `retry_override_input` 临时放行一个输入，却没有稳定恢复整个队列；
- `resume_session` 和 `resume_queued_input` 分担相近语义。

结果是用户中断后再发送一条消息，Session 仍被误判为“Runtime 重启后必须显式恢复”；或者指定输入
执行完成后，后续队列再次停住。根因不是驱动器没有 wake，而是两个不同用户意图共享了门禁写法。

## 三、权威状态

### 3.1 两个正交事实

```rust
pub struct SessionState {
    // ...
    pub(crate) queue_paused_by_user: bool,
    pub(crate) resume_required: bool,
}
```

- `queue_paused_by_user`：当前用户主动暂停自动串行。产生于中断、拒绝审批并停止；新提交输入或显式
  恢复代表用户继续操作，可以清除。
- `resume_required`：Runtime 启动恢复时发现 runnable 未完成输入，为避免重启后自动重放副作用而
  建立的安全门禁。只有显式恢复可以清除；新发消息不能代替对旧任务的恢复确认。

不把两者压成枚举，因为两者可能同时为真：例如 Runtime 重启后用户又对可见残留状态执行暂停。
派生给 UI 的优先级只是展示规则，不改变底层事实。

### 3.2 驱动判定

Session driver claim 下一输入的统一条件：

```text
存在 active run                       -> 不 claim
queue_paused_by_user                  -> 不 claim
resume_required                       -> 不 claim
存在 pending approval/其他既有门禁     -> 不 claim
否则                                  -> claim 队首 runnable input
```

删除 `retry_override_input` 对持久队列门禁的绕过用途。指定某项恢复改为先调整持久队列顺序，再清除门禁，
随后仍由同一个正常 driver claim 路径执行，避免形成第二套执行状态机。

## 四、状态转移

| 事件/命令 | `queue_paused_by_user` | `resume_required` | 队列顺序 | 是否 wake |
| --- | --- | --- | --- | --- |
| 正常启动、无 runnable | `false` | `false` | 不变 | 否 |
| 重启恢复、有 runnable | `false` | `true` | 持久顺序 | 否 |
| `interrupt_run` 成功 | `true` | 不变 | 不变 | 否 |
| `reject_approval_and_stop_run` | `true` | 不变 | 不变 | 否 |
| `submit_input` 成功 | `false` | 不变 | 新项入队尾 | 仅两个门禁都清除后 wake |
| `resume_queued_input(None)` | `false` | `false` | 不变 | 是 |
| `resume_queued_input(Some(id))` | `false` | `false` | id 移到 runnable 队首 | 是 |
| `prioritize_queued_input(id)` | 不变 | 不变 | id 移到 runnable 队首 | 仅原本可自动执行时 wake |
| `cancel_queued_input(id)` | 不变 | 不变 | 删除/标记取消 | 按既有规则 |

关键语义：

- 用户中断后发送新消息：清除用户暂停，旧输入与新输入按当前持久顺序继续自动串行。
- Runtime 重启后发送新消息：只清除可能存在的用户暂停，不清除 `resume_required`；仍需用户显式恢复。
- 指定输入恢复：把目标置顶并恢复整个队列，不是“只放行一条”。目标完成后后续输入继续执行。
- 仅置顶：不等同恢复。若 Session 正在暂停，置顶后仍暂停。

## 五、命令与协议收敛

### 5.1 统一恢复命令

把 `resume_queued_input` 改为可选目标：

```rust
pub struct ResumeQueuedInputRequest {
    pub session_id: SessionId,
    pub input_id: Option<InputId>,
}
```

- `None`：保持队列顺序，清除两个门禁，恢复整条队列；
- `Some(id)`：校验目标属于该 Session 且仍 runnable，在同一 mutation gate 中先持久置顶，然后清除
  两个门禁并 wake；
- 目标无效时不修改任何门禁或顺序；
- 返回最新 Queue/Session View 投影，不让 Desktop 乐观修改。

旧 `resume_session` 在 v0.16.0 保留传输兼容，Host 收到后内部转为 `resume_queued_input(None)`；
Desktop、生成协议示例和新测试不再调用旧命令。版本归档时标记 deprecated，后续协议破坏窗口再删除。

### 5.2 中断与拒绝

`interrupt_run` 和 `reject_approval_and_stop_run` 只设置 `queue_paused_by_user = true`，绝不写
`resume_required`。它们继续取消当前 Run/审批，并在相同 Session mutation gate 内结算状态。

如果取消结算或 Store 操作失败，不先写内存暂停值；保持持久化优先、内存随后投影的现有原则。

### 5.3 提交输入

`submit_input` 在输入事实成功持久化后把 `queue_paused_by_user` 清为 `false`，不改
`resume_required`。幂等重放同一 request 时不得重复插入，也不得把一次旧响应再次当成新的“继续”
意图；只有首次成功接受的提交执行清除。

清除后计算统一 driver 条件：若 `resume_required` 仍为真，只发状态更新，不 wake 执行；否则 wake。

### 5.4 投影

保留 `QueueExecutionState` 三种展示值：

```text
paused_by_user  if queue_paused_by_user
resume_required else if resume_required
automatic       otherwise
```

当两个字段同时为真，UI 优先显示“已由用户暂停”；显式恢复会同时清除。`SessionSummary.resume_required`
这类重复布尔投影应逐步收敛到 Queue Snapshot 的权威字段，但本版本为兼容可以保留并确保同源计算，
不能由不同代码路径分别赋值。

## 六、并发与原子性

所有相关命令继续进入 Session mutation gate。需要持久化顺序变化的操作必须满足：

1. 校验目标和当前 revision；
2. 先提交 Store 的队列顺序/Run 状态变化；
3. 成功后修改内存门禁；
4. 生成新 snapshot/revision；
5. gate 释放后 wake driver。

不要在持有同步锁时 await Store；沿用当前异步 gate/状态所有权模式。wake 是提示而不是业务事实，
即使重复或丢失，driver 也必须从权威队列和门禁重新判断，不能重复 claim。

取消、恢复和 submit 并发时，以进入 Session gate 的顺序线性化：

- resume 先完成、interrupt 后完成：最终用户暂停；
- interrupt 先完成、submit 后完成：最终用户暂停清除并自动执行，除非有重启门禁；
- submit 与 Runtime 恢复初始化不会并发写同一未完成 Session；初始化完成后才对外服务。

## 七、恢复与持久化

两个门禁当前属于 Runtime 恢复状态而非独立数据库列：`resume_required` 可由启动时 runnable 输入推导，
用户暂停是否需要跨进程保存遵循现有产品语义。本修复不为队列额外引入 schema；如果现有实现不持久化
用户暂停，Runtime 重启统一回到 `resume_required` 安全门禁即可。

启动恢复算法：

1. 精确读取该 Session 的 queued/runnable 输入和 active/未结算 Run；
2. 按现有恢复规则结算不可继续的 active Run；
3. 只要仍有需要用户决定是否继续的 runnable 输入，设置 `resume_required = true`；
4. 初始化 `queue_paused_by_user = false`，不把重启安全门禁伪装成用户暂停；
5. 在服务就绪前完成上述状态，避免 Desktop 先观察到短暂 automatic。

## 八、Desktop 行为

- `automatic`：Composer 正常发送；有队列时显示自动串行。
- `paused_by_user`：显示“已暂停”；用户发送新消息本身即可继续，也可点击“继续全部”。
- `resume_required`：显示“重启后等待继续”；发送新消息仍会入队但不自动执行，并明确提示需要点击继续。
- 每条 queued input 的“从此处继续”调用 `resume_queued_input(Some(id))`；“继续全部”调用 `None`。
- “置顶”继续只调用 prioritize，不偷偷恢复暂停队列。

前端不得在发送成功后本地把状态改成 automatic，必须使用 Runtime 返回投影；这样重启门禁不会被 UI
误清除。

## 九、回归矩阵

### 9.1 Runtime 单元/集成

1. 手动中断，提交新输入：用户暂停清除，旧/新输入按顺序全部完成。
2. 手动中断，点击继续全部：不改变顺序，全部完成。
3. 手动中断，从第二项继续：第二项置顶，随后其余项继续，不再次停住。
4. 重启恢复，提交新输入：新输入入队但仍不执行，`resume_required` 保持。
5. 重启恢复，继续全部：门禁清除且全部执行。
6. 两门禁同时存在，提交新输入：只清用户暂停；显式恢复后才执行。
7. paused 状态下仅 prioritize：顺序改变但不执行。
8. 无效 resume target：顺序、门禁、revision 均不变。
9. Store 置顶失败：门禁不清、driver 不 wake。
10. 重复 submit request：只接受一次，只产生一次继续意图。
11. interrupt/resume、interrupt/submit 并发：按 gate 顺序得到确定终态，无双 claim。
12. 拒绝审批并停止后新提交：与手动中断一致；拒绝审批普通模式不误暂停。

### 9.2 协议/Desktop

- 旧 `resume_session` 兼容转发与新命令结果一致；
- generated TypeScript 只有新 UI 使用可选 `input_id`；
- 三种状态文案、按钮和发送行为与矩阵一致；
- SSE 丢失/重连后通过 Session View 恢复正确状态；
- 现有正常自动队列、取消单项、置顶和审批流程无回归。

临时修复记录已在 M7 清理；长期设计只引用本附件，避免临时文档成为第二份权威。
